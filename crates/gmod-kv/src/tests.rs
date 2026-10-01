use super::*;

fn strings<'a>(values: impl Iterator<Item = &'a Value>) -> Vec<&'a str> {
    values.filter_map(Value::as_str).collect()
}

#[test]
fn material() {
    let text = r#"
"VertexLitGeneric"
{
	"$basetexture" "models\props_c17\furniture01"
	$surfaceprop wood // the comment is not a token
	"$color2" "[1 0.5 0]"

	"Proxies"
	{
		"Sine" { "resultVar" "$alpha" "sinemin" ".5" }
	}
}
"#;
    let doc = parse(text).unwrap();
    assert!(doc.directives.is_empty());
    let root = doc.root.block("vertexlitgeneric").expect("root block, found without regard to case");
    assert_eq!(root.str("$BaseTexture"), Some(r"models\props_c17\furniture01"));
    assert_eq!(root.str("$surfaceprop"), Some("wood"));
    assert_eq!(root.str("$color2"), Some("[1 0.5 0]"));
    let sine = root.block("proxies").and_then(|p| p.block("sine")).unwrap();
    assert_eq!(sine.str("resultvar"), Some("$alpha"));
    assert_eq!(sine.str("sinemin"), Some(".5"));
    assert_eq!(root.pairs[1].line, 5);
}

#[test]
fn escapes_only_when_asked() {
    let text = r#""LibraryFolders" { "path" "C:\\Program Files (x86)\\Steam" "q" "say \"hi\"" }"#;
    // Without escapes a backslash is an ordinary character and `\"` ends the string.
    let plain = parse(text).unwrap();
    let folders = plain.root.block("libraryfolders").unwrap();
    assert_eq!(folders.str("path"), Some(r"C:\\Program Files (x86)\\Steam"));
    assert_eq!(folders.str("q"), Some(r"say \"));

    let doc = parse_with(text, Options { escapes: true }).unwrap();
    let folders = doc.root.block("libraryfolders").unwrap();
    assert_eq!(folders.str("path"), Some(r"C:\Program Files (x86)\Steam"));
    assert_eq!(folders.str("q"), Some(r#"say "hi""#));

    let doc = parse(r#""a" { "path" "C:\steamcmd\cstrike" }"#).unwrap();
    assert_eq!(doc.root.block("a").unwrap().str("path"), Some(r"C:\steamcmd\cstrike"));
}

#[test]
fn conditions() {
    let text = r#"
"Resource"
{
	"pc"        "1" [$WIN32]
	"console"   "2" [$X360]
	"notxbox"   [!$X360] "3"
	"either"    "4" [$X360||$WIN32]
	"nested" [$WIN32]
	{
		"deep"  "5" [$GAMECONSOLE]
		"kept"  "6"
	}
}
"#;
    let mut doc = parse(text).unwrap();
    let res = doc.root.block("resource").unwrap();
    assert_eq!(res.pairs[0].condition.as_deref(), Some("[$WIN32]"));
    assert_eq!(res.pairs[2].condition.as_deref(), Some("[!$X360]"));
    assert_eq!(res.len(), 5);

    doc.root.apply_conditions(&pc_symbol);
    let res = doc.root.block("resource").unwrap();
    let keys: Vec<&str> = res.iter().map(|p| p.key.as_str()).collect();
    assert_eq!(keys, ["pc", "notxbox", "either", "nested"]);
    let nested = res.block("nested").unwrap();
    assert_eq!(nested.len(), 1);
    assert_eq!(nested.str("kept"), Some("6"));
}

#[test]
fn condition_expressions() {
    let pc = |s: &str| pc_symbol(s);
    assert!(eval_condition("[$WIN32]", &pc));
    assert!(!eval_condition("[$X360]", &pc));
    assert!(eval_condition("[!$X360]", &pc));
    assert!(eval_condition("[$X360 || $WIN32]", &pc));
    assert!(!eval_condition("[$WIN32 && $X360]", &pc));
    assert!(eval_condition("[$WIN32&&!$PS3]", &pc));
    assert_eq!(eval_condition("[$WINDOWS]", &pc), cfg!(windows));
}

#[test]
fn duplicate_keys_keep_their_order() {
    let doc = parse(r#""rndwave" { "wave" "a.wav" "Wave" "b.wav" "other" "x" "wave" "c.wav" }"#).unwrap();
    let block = doc.root.block("rndwave").unwrap();
    assert_eq!(strings(block.get_all("wave")), ["a.wav", "b.wav", "c.wav"]);
    assert_eq!(block.str("wave"), Some("a.wav"));
}

#[test]
fn several_top_level_blocks_and_directives() {
    let text = "#base \"base.res\"\n#INCLUDE scripts/more.txt\n\"one\" { \"a\" \"1\" }\ntwo\n{\n b 2\n}\n";
    let doc = parse(text).unwrap();
    assert_eq!(
        doc.directives,
        [
            Directive { kind: DirectiveKind::Base, path: "base.res".into(), line: 1 },
            Directive { kind: DirectiveKind::Include, path: "scripts/more.txt".into(), line: 2 },
        ]
    );
    assert_eq!(doc.root.block("one").unwrap().str("a"), Some("1"));
    assert_eq!(doc.root.block("two").unwrap().str("b"), Some("2"));
    assert_eq!(doc.root.pairs[1].line, 4);
}

#[test]
fn missing_brace_at_the_end_keeps_what_was_read() {
    let text = "\"root\"\n{\n\t\"a\" \"1\"\n\t\"inner\"\n\t{\n\t\t\"b\" \"2\"\n";
    let err = parse(text).unwrap_err();
    assert_eq!(err.line, 7);
    let (doc, errors) = parse_lenient(text, Options::default());
    assert_eq!(errors.len(), 2, "{errors:?}");
    let root = doc.root.block("root").unwrap();
    assert_eq!(root.str("a"), Some("1"));
    assert_eq!(root.block("inner").unwrap().str("b"), Some("2"));
}

#[test]
fn empty_key_ends_the_block_like_the_engine() {
    let text = r#""root" { "a" "1" "" "2" } "next" { "c" "3" }"#;
    let (doc, errors) = parse_lenient(text, Options::default());
    assert!(errors[0].message.contains("empty key"), "{errors:?}");
    let root = doc.root.block("root").unwrap();
    assert_eq!(root.len(), 1);
    // What follows the empty key is read by the level above, as the engine does.
    assert!(doc.root.get("2").is_some());
}

#[test]
fn key_without_value() {
    let (doc, errors) = parse_lenient(r#""root" { "a" "1" "b" } "next" { "c" "3" }"#, Options::default());
    assert_eq!(errors.len(), 1, "{errors:?}");
    let root = doc.root.block("root").unwrap();
    assert_eq!(root.str("b"), Some(""));
    assert_eq!(doc.root.block("next").unwrap().str("c"), Some("3"));
}

#[test]
fn top_level_string_value_is_an_error() {
    let (doc, errors) = parse_lenient(r#""a" "b" "c" { "d" "e" }"#, Options::default());
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(doc.root.block("a"), Some(&Block::default()));
    assert_eq!(doc.root.block("c").unwrap().str("d"), Some("e"));
}

#[test]
fn comments_and_tokens() {
    let text = "// header\n\"m\" // after the key\n{ // after the brace\n  \"url\" \"http://example.com/a//b\"\n  $alpha 1//x\n}\n";
    let doc = parse(text).unwrap();
    let m = doc.root.block("m").unwrap();
    assert_eq!(m.str("url"), Some("http://example.com/a//b"));
    // `//` inside an unquoted token belongs to the token, as in the engine
    assert_eq!(m.str("$alpha"), Some("1//x"));
}

#[test]
fn nul_ends_the_text() {
    let doc = parse("\"a\" { \"b\" \"c\" }\0\0garbage {").unwrap();
    assert_eq!(doc.root.len(), 1);
}

#[test]
fn unterminated_string() {
    let err = parse("\"a\"\n{\n\"b\" \"never closed\n}\n").unwrap_err();
    assert_eq!(err.line, 3);
}

#[test]
fn multi_line_strings_count_lines() {
    let doc = parse("\"a\"\n{\n\"text\" \"one\ntwo\"\n\"next\" \"x\"\n}").unwrap();
    let a = doc.root.block("a").unwrap();
    assert_eq!(a.str("text"), Some("one\ntwo"));
    assert_eq!(a.pairs[1].line, 5);
}

#[test]
fn utf16_and_bom() {
    let mut utf16 = vec![0xFF, 0xFE];
    for unit in "\"lang\" { \"Tokens\" { \"Hi\" \"Привет\" } }".encode_utf16() {
        utf16.extend_from_slice(&unit.to_le_bytes());
    }
    let text = decode_text(&utf16);
    let doc = parse(&text).unwrap();
    assert_eq!(doc.root.block("lang").unwrap().block("tokens").unwrap().str("hi"), Some("Привет"));

    assert_eq!(decode_text(b"\xEF\xBB\xBF\"a\" {}"), "\"a\" {}");
    assert_eq!(decode_text(b"plain"), "plain");
}

#[test]
fn display_round_trip() {
    let text = r#"#base "x.res"
"root" [$WIN32]
{
	"a" "1" [$X360]
	"b" { "c" "2" "c" "3" }
	"empty" { }
}
"second" { "k" "v" }
"#;
    let doc = parse(text).unwrap();
    let again = parse(&doc.to_string()).unwrap();
    assert_eq!(strip_lines(doc), strip_lines(again));
}

fn strip_lines(mut doc: Document) -> Document {
    fn block(b: &mut Block) {
        for p in &mut b.pairs {
            p.line = 0;
            if let Value::Block(inner) = &mut p.value {
                block(inner);
            }
        }
    }
    block(&mut doc.root);
    for d in &mut doc.directives {
        d.line = 0;
    }
    doc
}

#[test]
fn deep_nesting_is_cut_off_not_a_crash() {
    let depth = 100_000;
    let text = String::from("\"root\" {") + &"\"a\" {".repeat(depth) + &"}".repeat(depth) + "} \"after\" { \"k\" \"v\" }";
    let (doc, errors) = parse_lenient(&text, Options::default());
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].message.contains("nested deeper"));
    // what follows the deep part is still read
    assert_eq!(doc.root.block("after").unwrap().str("k"), Some("v"));
    // the levels up to the limit are kept; the block at the limit stays, empty
    let mut block = doc.root.block("root").unwrap();
    let mut levels = 1;
    while let Some(inner) = block.block("a") {
        block = inner;
        levels += 1;
    }
    assert_eq!(levels, MAX_DEPTH + 1);
    assert!(block.is_empty());
}
