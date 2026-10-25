//! XML as element records: parse_xml, select/select_one paths and to_xml.

use rill::types::Value;

const DOMAIN: &str = "tests/fixtures/xml/domain.xml";

fn run(body: &str) -> Value {
    let source = format!("let dom = path(\"{DOMAIN}\").read_text().unwrap().parse_xml()\n{body}");
    rill::run_source(&source, Vec::new()).expect(body)
}

/// The value as `print` shows it: lists as `[a, b]`, strings bare.
fn text(body: &str) -> String {
    run(body).to_string()
}

fn run_err(source: &str) -> (String, String) {
    let error = rill::run_source(source, Vec::new()).expect_err(source);
    (error.to_string(), error.help_text().unwrap_or_default().to_string())
}

#[test]
fn elements_are_records_with_tag_attrs_text_and_children() {
    assert_eq!(text("dom.tag"), "domain");
    assert_eq!(text("dom.keys()"), "[tag, attrs, text, children]");
    // Namespace declarations come first, then attributes in document order.
    assert_eq!(text("dom.attrs.keys()"), "[xmlns:qemu, type]");
    assert_eq!(text("dom.attrs.type"), "kvm");
    // Whitespace-only text is dropped and the comment is gone.
    assert_eq!(text("dom.text"), "");
    assert_eq!(
        text("dom.children.map(.tag)"),
        "[name, uuid, memory, vcpu, metadata, os, devices, qemu:commandline]"
    );
    assert_eq!(text("dom.children[0].text"), "vm1");
    assert_eq!(text("dom.select_one(\"os/boot\").unwrap().children"), "[]");
    assert_eq!(
        text("dom.select_one(\"metadata/note/text()\").unwrap()"),
        "uses <virtio> & friends"
    );
}

#[test]
fn paths_select_children_descendants_and_attributes() {
    assert_eq!(
        text(r#"dom.select("devices/disk/source/@file")"#),
        "[/var/lib/libvirt/images/vm1.qcow2, /var/lib/libvirt/images/installer.iso]"
    );
    assert_eq!(text(r#"dom.select("devices/*").len()"#), "6");
    assert_eq!(text(r#"dom.select("//source").len()"#), "4");
    assert_eq!(text(r#"dom.select("devices//@bus")"#), "[virtio, sata]");
    assert_eq!(text(r#"dom.select("./name/text()")"#), "[vm1]");
    assert_eq!(text(r#"dom.select("@type")"#), "[kvm]");
    assert_eq!(text(r#"dom.select("qemu:commandline/qemu:arg/@value")"#), "[-s]");
    // Elements without the attribute are skipped.
    assert_eq!(text(r#"dom.select("devices/interface/source/@bridge")"#), "[br0]");
}

#[test]
fn predicates_filter_matches() {
    assert_eq!(text(r#"dom.select("devices/disk[@device='cdrom']/target/@dev")"#), "[sda]");
    assert_eq!(text(r#"dom.select("devices/disk[@device=\"disk\"]/target/@dev")"#), "[vda]");
    assert_eq!(text(r#"dom.select("devices/*[@type]").len()"#), "5");
    assert_eq!(
        text(r#"dom.select("devices/disk[readonly]/source/@file")"#),
        "[/var/lib/libvirt/images/installer.iso]"
    );
    assert_eq!(text(r#"dom.select("devices/disk[2]/target/@dev")"#), "[sda]");
    assert_eq!(text(r#"dom.select("devices/disk[3]")"#), "[]");
    assert_eq!(
        text(r#"dom.select(".[name='vm1']/uuid/text()")"#),
        "[4dea22b3-1d52-d8f3-2516-782e98ab3fa0]"
    );
    assert_eq!(
        text(r#"dom.select("devices/interface[@type='bridge'][1]/mac/@address")"#),
        "[52:54:00:aa:bb:01]"
    );
}

#[test]
fn select_chains_over_lists_and_select_one_is_optional() {
    assert_eq!(
        text(r#"dom.select("devices/interface").select("mac/@address")"#),
        "[52:54:00:aa:bb:01, 52:54:00:aa:bb:02]"
    );
    assert_eq!(text(r#"dom.select_one("name/text()")"#), "some(vm1)");
    assert_eq!(text(r#"dom.select_one("devices/hostdev")"#), "none");
    assert_eq!(
        text(
            r#"dom.select("devices/interface").map(|i| i.select_one("source/@network").unwrap_or("-"))"#
        ),
        "[-, default]"
    );
}

#[test]
fn to_xml_round_trips_and_escapes() {
    assert_eq!(run("dom.to_xml().parse_xml() == dom"), Value::Bool(true));
    assert_eq!(
        text(r#"dom.select_one("devices/disk").unwrap().to_xml()"#),
        "<disk type=\"file\" device=\"disk\">\n  <driver name=\"qemu\" type=\"qcow2\"/>\n  <source file=\"/var/lib/libvirt/images/vm1.qcow2\"/>\n  <target dev=\"vda\" bus=\"virtio\"/>\n</disk>"
    );
    assert!(text("dom.to_xml()").contains("<note>uses &lt;virtio&gt; &amp; friends</note>"));
    assert!(text("dom.to_xml()").starts_with(
        "<domain xmlns:qemu=\"http://libvirt.org/schemas/domain/qemu/1.0\" type=\"kvm\">"
    ));
    // Hand-built records: missing keys are empty, values are stringified.
    assert_eq!(
        text(r#"{tag: "graphics", attrs: {port: 5902, title: "a \"b\" & c"}}.to_xml()"#),
        "<graphics port=\"5902\" title=\"a &quot;b&quot; &amp; c\"/>"
    );
    assert_eq!(text(r#"{tag: "name", text: "x<y"}.to_xml()"#), "<name>x&lt;y</name>");
}

#[test]
fn edits_through_nested_assignment_are_written_back() {
    let out = text(
        r#"var d = dom
d.attrs.type = "qemu"
d.children[0].text = "vm2"
d.to_xml().parse_xml().select("@type") + d.to_xml().parse_xml().select("name/text()")"#,
    );
    assert_eq!(out, "[qemu, vm2]");
}

#[test]
fn errors_explain_themselves() {
    let (message, help) = run_err(r#""<a><b></a>".parse_xml()"#);
    assert!(message.starts_with("cannot parse XML:"), "{message}");
    assert!(help.contains("well-formed XML"), "{help}");

    let (message, help) = run_err(r#""<a/>".parse_xml().select("b[@x='1'")"#);
    assert!(message.starts_with("bad XML path 'b[@x='1''"), "{message}");
    assert!(help.contains("a[@k='v']"), "{help}");
    let (message, _) = run_err(r#""<a/>".parse_xml().select("/a")"#);
    assert!(message.contains("leading /"), "{message}");
    let (message, _) = run_err(r#""<a/>".parse_xml().select("@x/b")"#);
    assert!(message.contains("nothing may follow"), "{message}");
    let (message, _) = run_err(r#""<a/>".parse_xml().select("b[0]")"#);
    assert!(message.contains("count from 1"), "{message}");

    let (message, help) = run_err(r#"{name: "x"}.select("a")"#);
    assert_eq!(message, "select needs an XML element, got Map");
    assert!(help.contains(r#""name": "x""#), "{help}");
    let (message, _) = run_err(r#"{name: "x"}.to_xml()"#);
    assert_eq!(message, "to_xml needs an XML element, got Map");
}

#[test]
fn the_checker_accepts_typical_use() {
    let source = r#"fn disks(xml: Str) -> List[Str] {
    let dom = xml.parse_xml()
    print(dom.tag)
    let first: Option[Str] = dom.select_one("devices/disk/source/@file")
    return dom.select("devices/disk/source/@file")
}
disks("<domain/>")"#;
    assert_eq!(rill::run_source(source, Vec::new()).expect("checks"), Value::list(Vec::new()));
}
