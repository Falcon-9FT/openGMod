use super::*;

#[test]
fn calls_come_back_as_their_objects_functions_and_arguments() {
    let message = format!("{BRIDGE}[\"lua\",\"Run\",0,[\"print(%s)\",\"a \\\"b\\\"\",3]]");
    let (object, function, callback, args) = parse_call(message.strip_prefix(BRIDGE).unwrap()).unwrap();
    assert_eq!((object.as_str(), function.as_str(), args.as_str()), ("lua", "Run", "[\"print(%s)\",\"a \\\"b\\\"\",3]"));
    assert_eq!(callback, None);
    // a function last waits for Lua's answer
    let (_, function, callback, args) = parse_call("[\"language\",\"Update\",12,[\"new_game\"]]").unwrap();
    assert_eq!((function.as_str(), callback, args.as_str()), ("Update", Some(12), "[\"new_game\"]"));
    assert!(parse_call("not a call").is_none());
}

#[test]
fn the_bridge_goes_first_into_documents() {
    let objects = vec![("lua".to_string(), vec!["Run".to_string(), "PlaySound".to_string()])];
    let js = bridge(&objects);
    assert!(js.contains("window[\"lua\"][\"Run\"]=function") && js.contains("window[\"lua\"][\"PlaySound\"]"));
    // after <head>, not before the doctype
    let html = with_bridge("<!DOCTYPE html><html><HEAD lang=\"en\"><title>x</title></head></html>", &objects);
    assert!(html.starts_with("<!DOCTYPE html><html><HEAD lang=\"en\"><script>"), "{html}");
    let bare = with_bridge("<!doctype html><p>hi", &objects);
    assert!(bare.starts_with("<!doctype html><script>"), "{bare}");
    assert!(with_bridge("<p>hi", &[]).starts_with("<script>"));
}

#[test]
fn strings_and_paths() {
    assert_eq!(js_string("a\"b\\c\n\u{2028}"), "\"a\\\"b\\\\c\\n\\u2028\"");
    assert_eq!(percent_decode("html/my%20page.html"), "html/my page.html");
    assert_eq!(percent_decode("100%"), "100%");
    assert_eq!(mime("html/js/menu/app.JS"), "text/javascript");
}
