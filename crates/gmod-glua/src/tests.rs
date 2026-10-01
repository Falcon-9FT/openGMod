use super::*;

fn tr(src: &str) -> String {
    translate_str(src).unwrap().into_owned()
}

/// Runs of spaces collapsed, for comparing operator rewrites without caring about padding.
fn squash(s: &str) -> String {
    s.split(' ').filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ")
}

fn lines(s: &str) -> usize {
    s.matches('\n').count()
}

#[test]
fn plain_lua_is_borrowed() {
    let src = "local x = 1\nif x ~= 2 and not y then print('a // b', \"/* c */\") end -- && || ! continue\n--[[ != ]] local s = [==[ && ]] ]==]\n";
    assert!(matches!(translate(src.as_bytes()).unwrap(), Cow::Borrowed(_)));
    assert!(!uses_extensions(src.as_bytes()));
    assert!(uses_extensions(b"if a != b then end"));
}

#[test]
fn operators() {
    assert_eq!(squash(&tr("if a != b && !c || d then end")), "if a ~= b and not c or d then end");
    assert_eq!(tr("a!=b"), "a~=b");
    assert_eq!(squash(&tr("x = !!y")), "x = not not y");
    assert_eq!(squash(&tr("if!x then end")), "if not x then end");
    assert_eq!(squash(&tr("return a&&b||c")), "return a and b or c");
}

#[test]
fn comments_keep_lines() {
    let src = "a = 1 // one\nb = 2 /* two\nthree */ c = 3\n--[[ != ]] d = '//'\n";
    let out = tr(src);
    assert_eq!(out, "a = 1  \nb = 2  \n c = 3\n--[[ != ]] d = '//'\n");
    assert_eq!(lines(&out), lines(src));
    assert_eq!(tr("x = a/**/b"), "x = a b");
    assert_eq!(tr("x = 4 / 2 // half"), "x = 4 / 2  ");
    assert_eq!(tr("a = 1\r\n/* x\r\ny */ b = 2"), "a = 1\r\n \r\n b = 2");
}

#[test]
fn continue_in_for() {
    let src = "for i = 1, 10 do\n\tif i % 2 == 0 then continue end\n\tlocal x = i\n\tprint(x)\nend\n";
    let expected =
        "for i = 1, 10 do do\n\tif i % 2 == 0 then goto __glua_continue_1 end\n\tlocal x = i\n\tprint(x)\nend ::__glua_continue_1:: end\n";
    assert_eq!(tr(src), expected);
}

#[test]
fn continue_in_nested_loops() {
    let src = "while a do\n\tfor k, v in pairs(t) do\n\t\tif v then continue end\n\tend\n\tif b then continue end\nend\n";
    let expected = "while a do do\n\tfor k, v in pairs(t) do do\n\t\tif v then goto __glua_continue_1 end\n\tend ::__glua_continue_1:: end\n\tif b then goto __glua_continue_2 end\nend ::__glua_continue_2:: end\n";
    assert_eq!(tr(src), expected);
}

#[test]
fn continue_before_return_and_with_semicolon() {
    let src = "for i = 1, 3 do\n\tif i == 1 then continue; end\n\treturn i\nend";
    assert_eq!(tr(src), "for i = 1, 3 do do\n\tif i == 1 then goto __glua_continue_1; end\n\treturn i\nend ::__glua_continue_1:: end");
}

#[test]
fn continue_in_repeat() {
    let src = "repeat\n\ti = i + 1\n\tif i < 5 then continue end\n\tprint(i)\nuntil i >= 10";
    assert_eq!(tr(src), "repeat\n\ti = i + 1\n\tif i < 5 then goto __glua_continue_1 end\n\tprint(i)\n::__glua_continue_1:: until i >= 10");
}

#[test]
fn loops_in_loop_headers() {
    // a function in a loop's head, with blocks of its own, does not confuse the loop's `do`
    let src = "for k, v in SortedPairs(t, function(a, b) do end return a < b end) do\n\tif !v then continue end\nend";
    let out = tr(src);
    assert!(out.starts_with("for k, v in SortedPairs(t, function(a, b) do end return a < b end) do do\n"), "{out}");
    assert!(out.ends_with("end ::__glua_continue_1:: end"), "{out}");
}

#[test]
fn continue_outside_a_loop() {
    let err = translate(b"if x then\n\tcontinue\nend").unwrap_err();
    assert_eq!(err.line, 2);
    let err = translate(b"for i = 1, 2 do\n\tlocal f = function()\n\t\tif x then continue end\n\tend\nend").unwrap_err();
    assert_eq!(err.line, 3);
}

#[test]
fn continue_as_a_name() {
    let src = "local t = { continue = 1 }\nt.continue = t.continue + 1\nlocal continue = 2\ngoto continue\n::continue::\nfunction obj:continue() end\nreturn continue end";
    assert_eq!(tr(src), src);
}

#[test]
fn strings_and_numbers_are_left_alone() {
    let src = "x = 0x1p4 + 1e-5 + .5 + 3 x = 'it\\'s && fine' .. \"\\\"!=\" .. [==[ || ]] ]==]\ns = \"a\\z\n   && b\"\n";
    assert_eq!(tr(src), src);
}

#[test]
fn bytes_that_are_not_utf8() {
    let src = b"print(\"\xcf\xf0\xe8\xe2\xe5\xf2\") // \xea\xee\xec\nif a != b then end";
    let out = translate(src).unwrap();
    assert_eq!(&out[..], b"print(\"\xcf\xf0\xe8\xe2\xe5\xf2\")  \nif a ~= b then end");
}

#[test]
fn byte_order_mark() {
    assert_eq!(tr("\u{feff}print(1)"), "print(1)");
}

#[test]
fn unfinished_block_comment() {
    let err = translate(b"a = 1\nb = 2 /* never closed\n").unwrap_err();
    assert_eq!(err.line, 2);
}

#[test]
fn unfinished_strings_are_left_to_luajit() {
    // the translation stops where LuaJIT will report the error itself
    let src = "x = 'unfinished\ny = a != b";
    assert_eq!(tr(src), src);
}

#[test]
fn lines_are_kept() {
    let src = "/* header\n * comment\n */\nlocal function f(t)\n\tfor _, v in ipairs(t) do // loop\n\t\tif !v then continue end\n\t\tif v != 1 && v != 2 then\n\t\t\tprint(v)\n\t\tend\n\tend\nend\n";
    let out = tr(src);
    assert_eq!(lines(&out), lines(src));
    for (a, b) in src.lines().zip(out.lines()) {
        // every original line is still on its own line: compare the words that are not GLua
        let keep = |s: &str| {
            s.split_whitespace().filter(|w| !["!=", "&&", "//", "/*", "*/", "*", "loop", "header", "comment"].contains(w)).count()
        };
        assert!(keep(b) >= keep(a).saturating_sub(2), "{a:?} vs {b:?}");
    }
}

/// `testdata/semantics.lua` exercises every addition. Its translation, run by LuaJIT 2.0
/// (which knows none of them), printed `testdata/semantics.expected` - the same as the
/// untranslated file under the LuaJIT 2.1 of 2026, which understands them natively. Until the
/// engine embeds LuaJIT, this test checks the translation's shape; `scripts/glua-semantics.sh`
/// repeats the run with a LuaJIT binary.
#[test]
fn semantics_sample() {
    let src = include_str!("../testdata/semantics.lua");
    let out = tr(src);
    assert_eq!(lines(&out), lines(src));
    for glua in ["!=", "&&", "||", "//", "/*"] {
        assert!(!out.contains(glua), "{glua} left in the output");
    }
    assert_eq!(out.matches("goto __glua_continue_").count(), 7);
    assert!(out.contains("local continue = t.continue"), "the name stays a name");
}

#[test]
fn define_baseclass() {
    assert_eq!(tr("DEFINE_BASECLASS( \"gamemode_base\" )\nx = 1"), "local BaseClass = baseclass.Get( \"gamemode_base\" )\nx = 1");
    assert_eq!(tr("DEFINE_BASECLASS(\"a\")"), "local BaseClass = baseclass.Get(\"a\")");
    // a field or a value of that name is not the macro
    for src in ["t.DEFINE_BASECLASS(\"a\")", "obj:DEFINE_BASECLASS(\"a\")", "local DEFINE_BASECLASS = 1", "print(DEFINE_BASECLASS)"] {
        assert_eq!(tr(src), src);
    }
}
