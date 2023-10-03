fn main() {
    let sysinfo = systeminfo::systeminfo().expect("failed to gather systeminfo");
    let json = serde_json::to_string_pretty(&sysinfo).unwrap();

    println!("{json}");
}
