#[test]
fn portable_build_script_writes_data_and_reconnect_readme() {
    let script = include_str!("../scripts/build-portable.ps1");

    assert!(script.contains("$portableReadme = @\""));
    assert!(script.contains("Set-Content"));
    assert!(script.contains("README.txt"));
    assert!(script.contains("data\\\\soziopolis_lingq_tool\\\\settings.json"));
    assert!(script.contains("data\\\\soziopolis_lingq_tool\\\\soziopolis_lingq_tool.db"));
    assert!(script.contains("LingQ token is stored in Windows Credential Manager"));
    assert!(script.contains("reconnected once on a new PC"));
}
