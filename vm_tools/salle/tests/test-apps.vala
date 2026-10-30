using Salle; // vala-lint=using-directive

/* Reading carte's app catalog and hidden list */

void test_parse () {
    var json = """{"vm":"debian","menu":true,"apps":[
        {"id":"org.gnome.Calculator","name":{"":"Calculator","de":"Rechner"},"comment":{"":"Math"},
         "icon_file":"/i/calc.svg"},
        {"id":"b","name":{"":"Abacus"}},
        {"id":"no-name","name":{}},
        {"name":{"":"No ID"}},
        42
    ]}""";
    var apps = AppCatalog.parse (json, "# hidden\nb\n\n", { "C" });
    assert (apps.length == 2);
    // Sorted by name
    assert (apps[0].id == "b" && apps[0].name == "Abacus" && apps[0].hidden);
    assert (apps[0].comment == null && apps[0].icon_file == null);
    assert (apps[1].name == "Calculator" && apps[1].comment == "Math" && !apps[1].hidden);
    assert (apps[1].icon_file == "/i/calc.svg");

    apps = AppCatalog.parse (json, "", { "de_DE.UTF-8", "de_DE", "de", "C" });
    assert (apps[1].name == "Rechner" && apps[1].comment == "Math");
}

void test_malformed () {
    foreach (var bad in new string[] { "", "[]", "{\"apps\":3}", "{\"apps\":[", "null" }) {
        assert (AppCatalog.parse (bad, "", { "C" }).length == 0);
    }
}

int main (string[] args) {
    Test.init (ref args);
    Test.add_func ("/apps/parse", test_parse);
    Test.add_func ("/apps/malformed", test_malformed);
    return Test.run ();
}
