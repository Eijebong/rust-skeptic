extern crate pulldown_cmark as cmark;
extern crate tempdir;

use std::env;
use std::fs::File;
use std::io::{self, Read, Write, Error as IoError};
use std::path::{PathBuf, Path};
use cmark::{Parser, Event, Tag};
use std::collections::HashMap;

pub fn generate_doc_tests<T: Clone>(docs: &[T]) where T : AsRef<str> {
    // This shortcut is specifically so examples in skeptic's on
    // readme can call this function in non-build.rs contexts, without
    // panicking below.
    if docs.is_empty() {
        return;
    }

    let docs = docs.iter().cloned().filter(|d| {
        !d.as_ref().ends_with(".skt.md")
    }).collect::<Vec<_>>();

    // Inform cargo that it needs to rerun the build script if one of the skeptic files are
    // modified
    for doc in &docs {
        println!("cargo:rerun-if-changed={}", doc.as_ref());
        println!("cargo:rerun-if-changed={}.skt.md", doc.as_ref());
    }

    let out_dir = env::var("OUT_DIR").unwrap();
    let cargo_manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();

    let mut out_file = PathBuf::from(out_dir.clone());
    out_file.push("skeptic-tests.rs");

    let config = Config {
        out_dir: PathBuf::from(out_dir),
        root_dir: PathBuf::from(cargo_manifest_dir),
        out_file: out_file,
        docs: docs.iter().map(|s| s.as_ref().to_string()).collect(),
    };

    run(config);
}

struct Config {
    out_dir: PathBuf,
    root_dir: PathBuf,
    out_file: PathBuf,
    docs: Vec<String>,
}

fn run(ref config: Config) {
    let tests = extract_tests(config).unwrap();
    emit_tests(config, tests).unwrap();
}

struct Test {
    name: String,
    text: Vec<String>,
    ignore: bool,
    no_run: bool,
    should_panic: bool,
    template: Option<String>,
}

struct DocTestSuite {
    doc_tests: Vec<DocTest>,
}

struct DocTest {
    path: PathBuf,
    old_template: Option<String>,
    tests: Vec<Test>,
    templates: HashMap<String, String>,
}

fn extract_tests(config: &Config) -> Result<DocTestSuite, IoError> {
    let mut doc_tests = Vec::new();
    for doc in &config.docs {
        let ref mut path = config.root_dir.clone();
        path.push(doc);
        let new_tests = try!(extract_tests_from_file(path));
        doc_tests.push(new_tests);
    }
    return Ok(DocTestSuite { doc_tests: doc_tests });
}

fn extract_tests_from_file(path: &Path) -> Result<DocTest, IoError> {
    let mut tests = Vec::new();
    // Oh this isn't actually a test but a legacy template
    let mut old_template = None;

    let mut file = try!(File::open(path));
    let ref mut s = String::new();
    try!(file.read_to_string(s));
    let parser = Parser::new(s);

    let mut test_name_gen = TestNameGen::new(path);
    let mut code_buffer = None;

    for event in parser {
        match event {
            Event::Start(Tag::CodeBlock(ref info)) => {
                let code_block_info = parse_code_block_info(info);
                if code_block_info.is_rust {
                    code_buffer = Some(Vec::new());
                }
            }
            Event::Text(text) => {
                if let Some(ref mut buf) = code_buffer {
                    buf.push(text.to_string());
                }
            }
            Event::End(Tag::CodeBlock(ref info)) => {
                let code_block_info = parse_code_block_info(info);
                if let Some(buf) = code_buffer.take() {
                    if code_block_info.is_old_template {
                        old_template = Some(buf.into_iter().collect())
                    } else {
                        tests.push(Test {
                            name: test_name_gen.advance(),
                            text: buf,
                            ignore: code_block_info.ignore,
                            no_run: code_block_info.no_run,
                            should_panic: code_block_info.should_panic,
                            template: code_block_info.template,
                        });
                    }
                }
            }
            _ => (),
        }
    }

    let templates = load_templates(path)?;

    Ok(DocTest {
        path: path.to_owned(),
        old_template: old_template,
        tests: tests,
        templates: templates,
    })
}

fn load_templates(path: &Path) -> Result<HashMap<String, String>, IoError> {
    let file_name = format!("{}.skt.md", path.file_name().expect("no file name").to_string_lossy());
    let path = path.with_file_name(&file_name);
    if !path.exists() {
        return Ok(HashMap::new());
    }

    let mut map = HashMap::new();

    let mut file = try!(File::open(path));
    let ref mut s = String::new();
    try!(file.read_to_string(s));
    let parser = Parser::new(s);

    let mut code_buffer = None;

    for event in parser {
        match event {
            Event::Start(Tag::CodeBlock(ref info)) => {
                let code_block_info = parse_code_block_info(info);
                if code_block_info.is_rust {
                    code_buffer = Some(Vec::new());
                }
            }
            Event::Text(text) => {
                if let Some(ref mut buf) = code_buffer {
                    buf.push(text.to_string());
                }
            }
            Event::End(Tag::CodeBlock(ref info)) => {
                let code_block_info = parse_code_block_info(info);
                if let Some(buf) = code_buffer.take() {
                    if let Some(t) = code_block_info.template {
                        map.insert(t, buf.into_iter().collect());
                    }
                }
            }
            _ => (),
        }
    }

    Ok(map)
}

struct TestNameGen {
    root: String,
    count: i32,
}

impl TestNameGen {
    fn new(path: &Path) -> TestNameGen {
        let ref file_stem = path.file_stem().unwrap().to_str().unwrap().to_string();
        TestNameGen {
            root: sanitize_test_name(file_stem),
            count: 0,
        }
    }

    fn advance(&mut self) -> String {
        let count = self.count;
        self.count += 1;
        format!("{}_{}", self.root, count)
    }
}

fn sanitize_test_name(s: &str) -> String {
    to_lowercase(s)
        .chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c
            } else {
                '_'
            }
        })
        .collect()
}

// Only converting test names to lowercase to avoid style lints
// against test functions.
fn to_lowercase(s: &str) -> String {
    use std::ascii::AsciiExt;
    // FIXME: unicode
    s.to_ascii_lowercase()
}

fn parse_code_block_info(info: &str) -> CodeBlockInfo {
    // Same as rustdoc
    let tokens = info.split(|c: char| !(c == '_' || c == '-' || c.is_alphanumeric()));

    let mut seen_rust_tags = false;
    let mut seen_other_tags = false;
    let mut info = CodeBlockInfo {
        is_rust: false,
        should_panic: false,
        ignore: false,
        no_run: false,
        is_old_template: false,
        template: None,
    };

    for token in tokens {
        match token {
            "" => {}
            "rust" => {
                info.is_rust = true;
                seen_rust_tags = true
            }
            "should_panic" => {
                info.should_panic = true;
                seen_rust_tags = true
            }
            "ignore" => {
                info.ignore = true;
                seen_rust_tags = true
            }
            "no_run" => {
                info.no_run = true;
                seen_rust_tags = true;
            }
            "skeptic-template" => {
                info.is_old_template = true;
                seen_rust_tags = true
            }
            _ if token.starts_with("skt-") => {
                info.template = Some(token[4..].to_string());
                seen_rust_tags = true;
            }
            _ => seen_other_tags = true,
        }
    }

    info.is_rust &= !seen_other_tags || seen_rust_tags;

    info
}

struct CodeBlockInfo {
    is_rust: bool,
    should_panic: bool,
    ignore: bool,
    no_run: bool,
    is_old_template: bool,
    template: Option<String>,
}

fn emit_tests(config: &Config, suite: DocTestSuite) -> Result<(), IoError> {
    let mut out = String::new();

    // Test cases use the api from skeptic::rt
    out.push_str("extern crate skeptic;\n");

    for doc_test in suite.doc_tests {
        for test in &doc_test.tests {
            let test_string = {
                if let Some(ref t) = test.template {
                    let template = doc_test.templates.get(t)
                        .expect(&format!("template {} not found for {}", t, doc_test.path.display()));
                    try!(create_test_runner(config, &Some(template.to_string()), test))
                } else {
                    try!(create_test_runner(config, &doc_test.old_template, test))
                }
            };
            out.push_str(&test_string);
        }
    }
    write_if_contents_changed(&config.out_file, &out)
}

/// Just like Rustdoc, ignore a "#" sign at the beginning of a line of code.
/// These are commonly an indication to omit the line from user-facing
/// documentation but include it for the purpose of playground links or skeptic
/// testing.
fn clean_omitted_line(line: &String) -> &str {
    let trimmed = line.trim_left();
    if trimmed == "#\n" {
        &trimmed[1..]
    } else if trimmed.starts_with("# ") {
        &trimmed[2..]
    } else {
        line
    }
}

/// Creates the Rust code that this test will be operating on.
fn create_test_input(lines: &[String]) -> String {
    lines.iter().map(clean_omitted_line).collect()
}

fn create_test_runner(config: &Config,
                      template: &Option<String>,
                      test: &Test)
                      -> Result<String, IoError> {

    let template = template.clone().unwrap_or_else(|| String::from("{}"));
    let test_text = create_test_input(&test.text);

    let mut s: Vec<u8> = Vec::new();
    if test.ignore {
        try!(writeln!(s, "#[ignore]"));
    }
    if test.should_panic {
        try!(writeln!(s, "#[should_panic]"));
    }

    try!(writeln!(s, "#[test] fn {}() {{", test.name));
    try!(writeln!(s,
                  "    let s = &format!(r####\"{}{}\"####, r####\"{}\"####);",
                  "\n",
                  template,
                  test_text));

    // if we are not running, just compile the test without running it
    if test.no_run {
        try!(writeln!(s,
            "    skeptic::rt::compile_test(r#\"{}\"#, s);",
            config.out_dir.to_str().unwrap()));
    } else {
        try!(writeln!(s,
            "    skeptic::rt::run_test(r#\"{}\"#, s);",
            config.out_dir.to_str().unwrap()));
    }

    try!(writeln!(s, "}}"));
    try!(writeln!(s, ""));

    Ok(String::from_utf8(s).unwrap())
}

fn write_if_contents_changed(name: &Path, contents: &str) -> Result<(), IoError> {
    // Can't open in write mode now as that would modify the last changed timestamp of the file
    match File::open(name) {
        Ok(mut file) => {
            let mut current_contents = String::new();
            try!(file.read_to_string(&mut current_contents));
            if current_contents == contents {
                // No change avoid writing to avoid updating the timestamp of the file
                return Ok(())
            }
        }
        Err(ref err) if err.kind() == io::ErrorKind::NotFound => (),
        Err(err) => return Err(err),
    }
    let mut file = try!(File::create(name));
    try!(file.write(contents.as_bytes()));
    Ok(())
}

pub mod rt {
    use std::env;
    use std::fs::{self, File};
    use std::io::{self, Write};
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::ffi::OsStr;
    use tempdir::TempDir;

    pub fn compile_test(out_dir: &str, test_text: &str) {
        let ref rustc = env::var("RUSTC").unwrap_or(String::from("rustc"));
        let ref outdir = TempDir::new("rust-skeptic").unwrap();
        let ref testcase_path = outdir.path().join("test.rs");
        let ref binary_path = outdir.path().join("out.exe");

        write_test_case(testcase_path, test_text);
        compile_test_case(testcase_path, binary_path, rustc, out_dir);
    }

    pub fn run_test(out_dir: &str, test_text: &str) {
        let ref rustc = env::var("RUSTC").unwrap_or(String::from("rustc"));
        let ref outdir = TempDir::new("rust-skeptic").unwrap();
        let ref testcase_path = outdir.path().join("test.rs");
        let ref binary_path = outdir.path().join("out.exe");

        write_test_case(testcase_path, test_text);
        compile_test_case(testcase_path, binary_path, rustc, out_dir);
        run_test_case(binary_path, outdir.path());
    }

    fn write_test_case(path: &Path, test_text: &str) {
        let mut file = File::create(path).unwrap();
        file.write_all(test_text.as_bytes()).unwrap();
    }

    fn compile_test_case(in_path: &Path, out_path: &Path, rustc: &str, out_dir: &str) {

        // FIXME: Hack. Because the test runner uses rustc to build
        // tests and those tests expect access to the crate this
        // project builds and its deps, we need to find the directory
        // containing Cargo's deps to pass as a `-L` flag to
        // rustc. Cargo does not give us this directly, but we know
        // relative to OUT_DIR where to look.
        let mut target_dir = PathBuf::from(out_dir);
        target_dir.pop();
        target_dir.pop();
        target_dir.pop();
        let mut deps_dir = target_dir.clone();
        deps_dir.push("deps");

        let mut cmd = Command::new(rustc);
        cmd.arg(in_path)
            .arg("--verbose")
            .arg("-o").arg(out_path)
            .arg("--crate-type=bin")
            .arg("-L").arg(target_dir)
            .arg("-L").arg(&deps_dir);

        for dep in fs::read_dir(deps_dir).expect("failed to access target/*/deps") {
            let dep = dep.expect("failed to read files from target/*/deps");
            let dep = dep.path();
            if let Some(name) = dep.file_stem().and_then(OsStr::to_str) {
                if let Some(ext) = dep.extension() {
                    if ext == "rlib" {
                        if let Some(libname) = name.rsplitn(2, '-').nth(1) {
                            let libname = &libname[3..];
                            cmd.arg("--extern");
                            cmd.arg(format!("{}={}", libname, dep.to_str().expect("filename not utf8")));
                        }
                    }
                }
            }
        }

        interpret_output(cmd);
    }

    fn run_test_case(program_path: &Path, outdir: &Path) {
        let mut cmd = Command::new(program_path);
        cmd.current_dir(outdir);
        interpret_output(cmd);
    }

    fn interpret_output(mut command: Command) {
        let output = command.output().unwrap();
        write!(io::stdout(),
               "{}",
               String::from_utf8(output.stdout).unwrap())
            .unwrap();
        write!(io::stderr(),
               "{}",
               String::from_utf8(output.stderr).unwrap())
            .unwrap();
        if !output.status.success() {
            panic!("Command failed:\n{:?}", command);
        }
    }
}

#[test]
fn test_omitted_lines() {
    let lines = &[
        "# use std::collections::BTreeMap as Map;\n".to_owned(),
        "#\n".to_owned(),
        "#[allow(dead_code)]\n".to_owned(),
        "fn main() {\n".to_owned(),
        "    let map = Map::new();\n".to_owned(),
        "    #\n".to_owned(),
        "    # let _ = map;\n".to_owned(),
        "}\n".to_owned(),
    ];

    let expected = [
        "use std::collections::BTreeMap as Map;\n",
        "\n",
        "#[allow(dead_code)]\n",
        "fn main() {\n",
        "    let map = Map::new();\n",
        "\n",
        "let _ = map;\n",
        "}\n",
    ].concat();

    assert_eq!(create_test_input(lines), expected);
}
