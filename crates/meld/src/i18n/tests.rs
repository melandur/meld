use super::plural::{self, Expr};
use super::po::{self, Syntax};
use super::*;

fn env(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let vars: Vec<(String, String)> = vars.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    move |name| vars.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
}

// --- .po parsing -------------------------------------------------------------

const SAMPLE: &str = r#"# German translation.
msgid ""
msgstr ""
"Language: de\n"
"Plural-Forms: nplurals=2; plural=(n != 1);\n"

#: meld/meldwindow.py:1
msgid "_Save"
msgstr "_Speichern"

#. Translators: a comment
#: meld/filediff.py:2
#, python-format
msgid "%d file"
msgid_plural "%d files"
msgstr[0] "%d Datei"
msgstr[1] "%d Dateien"

#: meld/resources/gtk/help-overlay.ui:3
msgctxt "shortcut window"
msgid "Quit Meld"
msgstr "Meld beenden"

#: meld/x.py:4
#, fuzzy
msgid "Fuzzy one"
msgstr "Unscharf"

#: meld/x.py:5
msgid ""
"Line one\n"
"Tab\there \"quoted\" back\\slash"
msgstr "Zeile eins\nTab\tda „zitiert“ \\ \101"

#: meld/x.py:6
#, python-format
msgid "%(name)s (%(cmd)s not installed)"
msgstr "%(name)s (%(cmd)s nicht installiert)"

#: meld/x.py:7
#, python-brace-format
msgid "{vc} comparison:"
msgstr "{vc}-Vergleich:"

#: meld/x.py:8
msgid "Untranslated"
msgstr ""

#: meld/x.py:9
#, python-format
msgid "%s vs %s"
msgstr "%s gegen %s mit %s"

#~ msgid "Obsolete"
#~ msgstr "Veraltet"
"#;

#[test]
fn parses_entries() {
    let file = po::parse(SAMPLE).unwrap();
    assert!(file.header_text().contains("Plural-Forms"));
    assert_eq!(file.entries.len(), 9);
    let save = &file.entries[0];
    assert_eq!((save.id.as_str(), save.strs.as_slice()), ("_Save", ["_Speichern".to_string()].as_slice()));
    let files = &file.entries[1];
    assert_eq!(files.id_plural.as_deref(), Some("%d files"));
    assert_eq!(files.strs, ["%d Datei", "%d Dateien"]);
    assert_eq!(files.flags, ["python-format"]);
    assert!(files.comments.iter().any(|c| c == "#. Translators: a comment"));
    assert_eq!(file.entries[2].context.as_deref(), Some("shortcut window"));
    assert!(file.entries[3].fuzzy());
    let escapes = &file.entries[4];
    assert_eq!(escapes.id, "Line one\nTab\there \"quoted\" back\\slash");
    assert_eq!(escapes.strs[0], "Zeile eins\nTab\tda „zitiert“ \\ A");
    assert!(!file.entries.iter().any(|e| e.id == "Obsolete"));
}

#[test]
fn reports_parse_errors() {
    assert_eq!(po::parse("msgid \"a\"\nmsgstr \"b\n").unwrap_err().line, 2);
    assert!(po::parse("msgstr \"b\"\n").is_err());
    assert!(po::parse("msgid \"a\"\nmsgstr \"\\q\"\n").is_err());
    assert!(po::parse("bogus \"a\"\n").is_err());
}

#[test]
fn builds_catalog() {
    let (catalog, skipped) = po::catalog(&po::parse(SAMPLE).unwrap()).unwrap();
    assert_eq!((catalog.nplurals, catalog.plural.as_str()), (2, "(n != 1)"));
    let get = |ctx: &str, id: &str| {
        catalog.messages.iter().find(|m| m.context == ctx && m.id == id).map(|m| m.forms.clone())
    };
    // Mnemonics dropped from both sides; the original spelling still works
    assert_eq!(get("", "Save"), Some(vec!["Speichern".to_string()]));
    assert_eq!(get("", "_Save"), Some(vec!["Speichern".to_string()]));
    assert_eq!(get("", "{} file"), Some(vec!["{} Datei".to_string(), "{} Dateien".to_string()]));
    assert_eq!(get("shortcut window", "Quit Meld"), Some(vec!["Meld beenden".to_string()]));
    assert_eq!(get("", "Quit Meld"), None);
    assert_eq!(get("", "Fuzzy one"), None);
    assert_eq!(get("", "Untranslated"), None);
    assert_eq!(get("", "{name} ({cmd} not installed)"), Some(vec!["{name} ({cmd} nicht installiert)".to_string()]));
    assert_eq!(get("", "{vc} comparison:"), Some(vec!["{vc}-Vergleich:".to_string()]));
    // Uses an argument the message doesn't have
    assert_eq!(get("", "{} vs {}"), None);
    assert_eq!(skipped, po::Skipped { fuzzy: 1, untranslated: 1, malformed: 1 });
}

#[test]
fn converts_python_syntax() {
    let p = |s| po::to_rust(s, Syntax::Percent);
    assert_eq!(p("%s and %d%%").as_deref(), Some("{} and {}%"));
    assert_eq!(p("%(name)s: %.1f {x}").as_deref(), Some("{name}: {} {{x}}"));
    assert_eq!(p("%2$s before %1$s").as_deref(), Some("{1} before {0}"));
    assert_eq!(p("100%!"), None);
    let b = |s| po::to_rust(s, Syntax::Brace);
    assert_eq!(b("{name!r:>5} {} {0} {{lit}}").as_deref(), Some("{name} {} {0} {{lit}}"));
    assert_eq!(b("bad }"), None);
    assert_eq!(po::to_rust("{a} 50%", Syntax::Literal).as_deref(), Some("{{a}} 50%"));
    assert_eq!(po::canonical("{0} of {1}"), "{} of {}");
    assert_eq!(po::canonical("{1} of {0}"), "{1} of {0}");
}

#[test]
fn strips_mnemonics() {
    assert_eq!(po::strip_mnemonic("_Save").as_deref(), Some("Save"));
    assert_eq!(po::strip_mnemonic("Save _As…").as_deref(), Some("Save As…"));
    assert_eq!(po::strip_mnemonic("保存(_S)").as_deref(), Some("保存"));
    assert_eq!(po::strip_mnemonic("Ouvrir (_O)").as_deref(), Some("Ouvrir"));
    assert_eq!(po::strip_mnemonic("Plain"), None);
    // Underscores in placeholders and doubled ones aren't mnemonics
    assert_eq!(po::strip_mnemonic("{file_name} _Open").as_deref(), Some("{file_name} Open"));
    assert_eq!(po::strip_mnemonic("{file_name}"), None);
    assert_eq!(po::strip_mnemonic("a__b _c").as_deref(), Some("a_b c"));
    // Several single underscores: an identifier, not a mnemonic
    assert_eq!(po::strip_mnemonic("_a _b"), None);
}

#[test]
fn encodes_and_decodes() {
    let (catalog, _) = po::catalog(&po::parse(SAMPLE).unwrap()).unwrap();
    let bytes = po::encode(&catalog);
    let decoded = po::decode(&bytes).unwrap();
    assert_eq!(decoded.plural, catalog.plural);
    assert_eq!(decoded.messages.len(), catalog.messages.len());
    assert!(po::decode(b"garbage").is_none());
    assert!(po::decode(&bytes[..bytes.len() - 1]).is_none());
}

// --- Plural forms --------------------------------------------------------------

#[test]
fn plural_expressions() {
    let eval = |src: &str, n| Expr::parse(src).unwrap().eval(n);
    assert_eq!((eval("n != 1", 1), eval("n != 1", 0), eval("n != 1", 5)), (0, 1, 1));
    assert_eq!((eval("(n > 1)", 0), eval("(n > 1)", 1), eval("(n > 1)", 2)), (0, 0, 1));
    assert_eq!(eval("0", 7), 0);
    // Russian/Ukrainian
    let slavic = "(n%10==1 && n%100!=11 ? 0 : n%10>=2 && n%10<=4 && (n%100<10 || n%100>=20) ? 1 : 2)";
    let forms: Vec<u64> = [1, 2, 5, 11, 21, 22, 25, 111, 104].iter().map(|&n| eval(slavic, n)).collect();
    assert_eq!(forms, [0, 1, 2, 2, 0, 1, 2, 2, 1]);
    // Irish style with nested conditionals
    let ga = "n==1 ? 0 : n==2 ? 1 : n>=3 && n<=10 ? 2 : 3";
    assert_eq!([1, 2, 7, 11].map(|n| eval(ga, n)), [0, 1, 2, 3]);
    assert_eq!(eval("!(n == 1) + 2 * 3 - 6 / 2 % 4", 1), 3);
    assert_eq!(eval("n / 0 + n % 0", 3), 0);
    assert!(Expr::parse("n ==").is_err());
    assert!(Expr::parse("(n").is_err());
    assert!(Expr::parse("x").is_err());
}

#[test]
fn plural_header() {
    assert_eq!(plural::from_header("Plural-Forms: nplurals=2; plural=(n != 1);\n"), Some((2, "(n != 1)".into())));
    assert_eq!(plural::from_header("Language: ja\nPlural-Forms: nplurals=1; plural=0\n"), Some((1, "0".into())));
    assert_eq!(plural::from_header("Language: x\n"), None);
}

/// Every embedded catalog's plural expression works.
#[test]
fn every_catalog_loads() {
    assert!(available().count() > 40);
    for code in available() {
        let lang = Language::load(code).unwrap_or_else(|| panic!("{code} doesn't load"));
        assert!(lang.messages.len() > 10, "{code}: {} messages", lang.messages.len());
        for n in 0..200 {
            assert!(lang.form(n) < lang.nplurals.max(1));
        }
    }
}

// --- Language selection -----------------------------------------------------

#[test]
fn languages_from_environment() {
    let w = |vars: &[(&str, &str)]| wanted_languages(env(vars));
    assert_eq!(w(&[("LANG", "pt_BR.UTF-8")]), ["pt_BR", "pt"]);
    assert_eq!(w(&[("LANGUAGE", "de:fr_CA"), ("LANG", "es_ES.UTF-8")]), ["de", "fr_CA", "fr", "es_ES", "es"]);
    assert_eq!(w(&[("LC_ALL", "sr_RS.UTF-8@latin"), ("LANG", "de")]), ["sr_RS@latin", "sr_RS", "sr@latin", "sr"]);
    assert_eq!(w(&[("LC_MESSAGES", "fr_FR"), ("LANG", "de_DE")]), ["fr_FR", "fr"]);
    assert_eq!(w(&[("LANGUAGE", ""), ("LANG", "C.UTF-8")]), ["C"]);
    assert_eq!(w(&[]), Vec::<String>::new());
}

#[test]
fn picks_catalogs() {
    let t = |vars: &[(&str, &str)]| Translator::for_languages(&wanted_languages(env(vars))).language();
    assert_eq!(t(&[("LANG", "pt_BR.UTF-8")]), Some("pt_BR"));
    assert_eq!(t(&[("LANG", "pt_PT.UTF-8")]), Some("pt"));
    assert_eq!(t(&[("LANG", "de_AT.UTF-8")]), Some("de"));
    assert_eq!(t(&[("LANG", "sr_RS@latin")]), Some("sr@latin"));
    assert_eq!(t(&[("LANGUAGE", "xx:fr"), ("LANG", "de_DE")]), Some("fr"));
    // English stops the search
    assert_eq!(t(&[("LANGUAGE", "en:de")]), None);
    assert_eq!(t(&[("LANG", "en_US.UTF-8")]), None);
    assert_eq!(t(&[("LANG", "C")]), None);
}

// --- Lookup and formatting ---------------------------------------------------

fn lang(code: &str) -> Translator {
    Translator::for_languages(&[code.to_string()])
}

#[test]
fn falls_back_through_languages() {
    let t = Translator::for_languages(&["xx".into(), "de".into()]);
    assert_eq!(t.lookup("", "Save"), "Speichern");
    let english = Translator::english();
    assert_eq!(english.lookup("", "Save"), "Save");
    assert_eq!(english.lookup_plural("", "{} file", "{} files", 1), "{} file");
    assert_eq!(english.lookup_plural("", "{} file", "{} files", 2), "{} files");
    assert_eq!(t.lookup("", "Not a message of Meld"), "Not a message of Meld");
}

/// Messages from the Python version resolve with its translations.
#[test]
fn python_messages_resolve() {
    let de = lang("de");
    assert_eq!(de.lookup("", "Save"), "Speichern");
    assert_eq!(de.lookup("", "Preferences"), "Einstellungen");
    assert_eq!(de.lookup("", "Folder {} is being compared to itself"), "Der Ordner »{}« wird mit sich selbst verglichen");
    assert_eq!(de.lookup("shortcut window", "Quit Meld"), "Meld beenden");
    assert_eq!(de.lookup_plural("", "{} branch", "{} branches", 1), "{} Zweig");
    assert_eq!(de.lookup_plural("", "{} branch", "{} branches", 3), "{} Zweige");
    let fr = lang("fr");
    assert_eq!(fr.lookup("", "Preferences"), "Préférences");
    assert_eq!(fr.lookup("", "Save"), "Enregistrer");
    let es = lang("es");
    assert_eq!(es.lookup("", "Preferences"), "Preferencias");
    assert_eq!(
        format(es.lookup("", "{vc} comparison:"), &[], &[("vc", &"Git")]),
        format(es.lookup("", "{vc} comparison:"), &[], &[("vc", &"Git")])
    );
    assert_ne!(es.lookup("", "{vc} comparison:"), "{vc} comparison:");
    // Japanese mnemonics in parentheses are dropped
    let ja = lang("ja");
    assert!(!ja.lookup("", "Save").contains("(_"), "{}", ja.lookup("", "Save"));
    // Russian has three forms
    let ru = lang("ru");
    let forms: Vec<&str> = [1, 2, 5].iter().map(|&n| ru.lookup_plural("", "{} branch", "{} branches", n)).collect();
    assert_eq!(forms.iter().collect::<std::collections::BTreeSet<_>>().len(), 3, "{forms:?}");
}

#[test]
fn formats_placeholders() {
    assert_eq!(format("{} of {}", &[&1, &"two"], &[]), "1 of two");
    // Translations may reorder
    assert_eq!(format("{1} von {0}", &[&1, &"two"], &[]), "two von 1");
    assert_eq!(format("{b} {a} {}", &[&0], &[("a", &"A"), ("b", &"B")]), "B A 0");
    assert_eq!(format("{{literal}} {}%", &[&50], &[]), "{literal} 50%");
    // Missing values stay visible rather than panicking
    assert_eq!(format("{} {} {x}", &[&1], &[]), "1 {} {x}");
    assert_eq!(format("unbalanced {", &[], &[]), "unbalanced {");
}

#[test]
fn macros() {
    // The test environment may set any language; check the shapes
    let plain: &'static str = tr!("Save");
    assert!(!plain.is_empty());
    let braces: &'static str = tr!("Use {{file}}");
    if language().is_none() {
        assert_eq!(plain, "Save");
        assert_eq!(braces, "Use {file}");
        assert_eq!(tr!("{} of {total}", 3, total = 5), "3 of 5");
        assert_eq!(tr!("{} and {}", "a", "b",), "a and b");
        assert_eq!(trn!("{} file", "{} files", 1, 1), "1 file");
        assert_eq!(trn!("{} file", "{} files", 2usize, 2), "2 files");
        assert_eq!(trn!("One file", "Many files", 7), "Many files");
        assert_eq!(trc!("shortcut window", "Quit Meld"), "Quit Meld");
        assert_eq!(trc!("ctx", "{name}!", name = "x"), "x!");
        assert_eq!(n_!("Label"), "Label");
    }
}
