use super::*;
use crate::pe::tests::{image, tsw_version};
use crate::target::{mismatches, Filter};

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("uniloader-facts-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// A Steam install laid out like Train Sim World 7, with the synthetic executable.
fn tsw_install(root: &Path) -> PathBuf {
    let game = root.join("steamapps/common/Train Sim World 7/WindowsNoEditor");
    let win64 = game.join("TS2Prototype/Binaries/Win64");
    std::fs::create_dir_all(&win64).unwrap_or_else(|e| panic!("create dirs: {e}"));
    std::fs::create_dir_all(game.join("Engine")).unwrap_or_else(|e| panic!("create dirs: {e}"));
    let exe = win64.join("TrainSimWorld.exe");
    std::fs::write(&exe, image(&tsw_version())).unwrap_or_else(|e| panic!("write exe: {e}"));
    std::fs::write(
        root.join("steamapps/appmanifest_4678800.acf"),
        "\"AppState\"\n{\n\t\"appid\"\t\t\"4678800\"\n\t\"name\"\t\t\"Train Sim World 7\"\n\
         \t\"buildid\"\t\t\"25890949\"\n\t\"installdir\"\t\t\"Train Sim World 7\"\n}\n",
    )
    .unwrap_or_else(|e| panic!("write manifest: {e}"));
    std::fs::write(
        root.join("steamapps/appmanifest_1.acf"),
        "\"AppState\"\n{\n\t\"appid\"\t\t\"1\"\n\t\"installdir\"\t\t\"Other\"\n}\n",
    )
    .unwrap_or_else(|e| panic!("write manifest: {e}"));
    exe
}

fn get<'a>(facts: &'a Facts, name: &str) -> Option<&'a Fact> {
    facts.get(name)
}

#[test]
fn gathers_a_tsw_like_install() {
    let root = temp("tsw");
    let exe = tsw_install(&root);
    let facts = gather(&exe, true);
    let t = |s: &str| Some(Fact::Text(s.to_owned()));
    assert_eq!(get(&facts, "exe.name").cloned(), t("TrainSimWorld.exe"));
    assert_eq!(get(&facts, "exe.folder").cloned(), t("Win64"));
    assert_eq!(get(&facts, "game.folder").cloned(), t("Train Sim World 7"));
    assert_eq!(get(&facts, "steam.app_id"), Some(&Fact::Number(4_678_800)));
    assert_eq!(
        get(&facts, "steam.build_id"),
        Some(&Fact::Number(25_890_949))
    );
    assert_eq!(get(&facts, "pe.machine"), Some(&Fact::Number(0x8664)));
    assert_eq!(get(&facts, "pe.is_dll").cloned(), t("false"));
    assert_eq!(
        get(&facts, "pe.version.CompanyName").cloned(),
        t("Dovetail Games")
    );
    assert_eq!(
        get(&facts, "pe.version.040904b0.ProductName").cloned(),
        t("TrainSimWorld7")
    );
    assert_eq!(
        get(&facts, "pe.fixed.file_version"),
        Some(&Fact::Version(vec![4, 26, 1, 0]))
    );
    assert_eq!(get(&facts, "unreal.engine_major"), Some(&Fact::Number(4)));
    assert_eq!(get(&facts, "unreal.branch").cloned(), t("Release-4.26"));
    assert_eq!(get(&facts, "unreal.changelist"), Some(&Fact::Number(0)));
    assert_eq!(
        get(&facts, "unreal.version"),
        Some(&Fact::Version(vec![4, 26, 1]))
    );
    assert_eq!(get(&facts, "unreal.project").cloned(), t("TS2Prototype"));
    assert!(
        matches!(get(&facts, "file.sha256"), Some(Fact::Text(h)) if h.len() == 64),
        "{facts:?}"
    );
    assert_eq!(get(&facts, "unity.backend"), None);

    let filter = |property: &str, values: &[&str]| Filter {
        property: property.to_owned(),
        values: values.iter().map(|v| (*v).to_owned()).collect(),
    };
    let tsw = [
        filter("exe.name", &["TrainSimWorld.exe"]),
        filter("pe.version.CompanyName", &["Dovetail Games"]),
        filter(
            "pe.version.ProductName",
            &["TrainSimWorld5", "TrainSimWorld7"],
        ),
        filter("unreal.project", &["TS2Prototype"]),
        filter("unreal.version", &[">=4.26 <4.27"]),
    ];
    assert_eq!(mismatches(&tsw, &facts), Vec::<String>::new());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn hash_only_when_asked_and_missing_files_give_names_only() {
    let root = temp("plain");
    std::fs::create_dir_all(&root).unwrap_or_else(|e| panic!("create dir: {e}"));
    let exe = root.join("Game.exe");
    std::fs::write(&exe, b"not a PE file").unwrap_or_else(|e| panic!("write: {e}"));
    std::fs::write(root.join("steam_appid.txt"), "480\n").unwrap_or_else(|e| panic!("write: {e}"));
    let facts = gather(&exe, false);
    assert_eq!(get(&facts, "file.sha256"), None);
    assert_eq!(get(&facts, "file.size"), Some(&Fact::Number(13)));
    assert_eq!(get(&facts, "steam.app_id"), Some(&Fact::Number(480)));
    assert_eq!(get(&facts, "pe.machine"), None);
    assert_eq!(get(&facts, "unreal.project"), None);
    assert_eq!(
        sha256(&exe).as_deref(),
        Some("1295384cbda4abca3518a550dae517d9d5381a330750f6c680665bfad51e2b45")
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn unreal_build_strings() {
    assert_eq!(
        unreal_build("++UE5+Release-5.3-CL-29314046"),
        Some((5, "Release-5.3".to_owned(), Some(29_314_046)))
    );
    assert_eq!(
        unreal_build("++UE4+Release-4.26"),
        Some((4, "Release-4.26".to_owned(), None))
    );
    assert_eq!(unreal_build("1.0.0"), None);
}

/// Print every property of a real executable: `UNILOADER_FACTS_SAMPLE=path cargo test -p
/// uniloader-core print_sample -- --ignored --nocapture`. Read only.
#[test]
#[ignore = "needs a real executable"]
fn print_sample() {
    let Ok(path) = std::env::var("UNILOADER_FACTS_SAMPLE") else {
        return;
    };
    for (name, fact) in gather(Path::new(&path), false) {
        println!("{name} = {fact}");
    }
}
