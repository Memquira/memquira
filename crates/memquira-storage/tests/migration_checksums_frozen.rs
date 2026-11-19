//! Applied migrations are identified by their checksum. Any edit to a shipped migration file (even a comment,
//! even a product rename) makes existing databases refuse to start, so these values are frozen: they are
//! the checksums 0.2.x recorded. Add new migrations as new files; never edit these.

#[allow(dead_code)]
const SQLITE: &[(i64, &str)] = &[
    (
        1,
        "dd686d121a1fe262d3cecb83bc8b43f1a66de42509e56630aae406ff38071bc9",
    ),
    (
        2,
        "1c223fc6b4339da514592f054ea2a2a4d69c873c47969c758c22f9123dc4b3f8",
    ),
    (
        3,
        "c4b9f53f30b1e0763fc058efa343472119103bccfb5d96efb16c6783797a52dc",
    ),
    (
        4,
        "41e9edeacd92abb0c514499faa99cf82537307799aa9d5afdab553782253152e",
    ),
    (
        5,
        "f6319318f94759f29695aa5bf2da8975c8bbbef42393b32cc8e787c0eaec61cf",
    ),
    (
        6,
        "3eb58484dbc085ae446cee094d8c0aea88b28699e06eff453344aa3b6ad87d14",
    ),
    (
        7,
        "35bea43c10c092b09add8248d49a23d1dd4f2a94f31e77dfff7593f8cd58d3d0",
    ),
    (
        8,
        "2b6f8fa94a4719683ed6a9a8793127ccaa4ffecf0c1f1747cc405f773449544b",
    ),
    (
        9,
        "d5a991c3b24f89daf5b56349d62605925bb8c068d0e3858667c6513e0ae7a7f9",
    ),
    (
        10,
        "1c91dacec8316c2ef064d0ec3677cf507cee6d8d367608f6e37100d4380c858e",
    ),
    (
        11,
        "ac29fa1f7d359347b4015aa5913c412b0653c7d4a764d2c6bd088ffbd3b28af4",
    ),
    (
        12,
        "dce549e6af467613c0307e78b83e11574e1dfcc59b1ff6d774f1b04ac743c329",
    ),
    (
        13,
        "6974cbacfbe838953c4e37f969f9705b22882aad7e25c9994f214b478a963bac",
    ),
    (
        14,
        "1d675f3275ff733882f78946fa0bdfd90d8057e3ebd76366fb72e5519c981c4e",
    ),
    (
        15,
        "78621de4a11262fd0a467d73abcf605f33f40b7ff346bba729c0af3e586f3f27",
    ),
    (
        16,
        "e2548c93551cd1d1e198e6c3bd6c4820c0f53ec1affc2b00d0ef4b4d55dee857",
    ),
    (
        17,
        "426ad614b1abba71b52002f8c1bd3cec112d5c661fcce377d5f89dc0025c5bd8",
    ),
];

#[allow(dead_code)]
const POSTGRES: &[(i64, &str)] = &[
    (
        1,
        "00eb6417f27ae5d0c98b49ad6e0b075fbfe20524771aa70f95ff457ad4c3c6d4",
    ),
    (
        2,
        "8fdc00a6fd392ddc6fa72ddc996f0d81d3b551f1399b0a8eaa9edd5fd2c7a0eb",
    ),
    (
        3,
        "38bf0826759fcbc01ca22328d62d5d572e3e12cf3ed630c18eefd3f6078adb10",
    ),
    (
        4,
        "21bf52be556238fc023b7bdffa55a7af2dc8110a71086e8621623b8be405b012",
    ),
    (
        5,
        "d00dfbb69387b64f356f12e77c3e1a090761bdf4cc50a7410533a24d99c55fee",
    ),
    (
        6,
        "400d79e82be00bc7a73449408cb38907f89210a1293cc14cc5adfc828d01fb60",
    ),
    (
        7,
        "bd0c468b84cc65f0bf925b2d8bae14e82e8ae08e22940fc69ca24a1b64f215fa",
    ),
    (
        8,
        "74d74a634b51b812a8ec8da24fea8ec938370390df5ed772798ff3726b7ab9b9",
    ),
    (
        9,
        "d1bf695345699267e611ad94a45b30ba4e7df5339d7ca098d1498ea47abc674c",
    ),
    (
        10,
        "6445e21159b173fc3aad06b1248ce9d11609c8bffeb9cbae640ae68b1b9eda14",
    ),
    (
        11,
        "29de6f635e6320dcb92418e810089c1edfa0adb223b745810ecf9e69cbb72bb6",
    ),
    (
        12,
        "6b5a2ebfb21d2a9f5d59dbd78437e6d8df0e978455f8c5c5df56dfa096ce4ae6",
    ),
    (
        13,
        "6a328385c9722a36aa7a3b44ed2ccdf509f187bc3e58c6bef3a05669e73c24d4",
    ),
    (
        14,
        "dcd709eb1e32fbe6baf9069033d330916ea542f41719e93f0314ae5e5382701f",
    ),
    (
        15,
        "c2bdf3ea4b79a4bc4330bde04f1081754f995cd37661dd281009daf263fb3563",
    ),
    (
        16,
        "9bf0893ba9c5d9e7c6371f391ab646d7c0b35eb3d883e8f02636ca3e209af8a2",
    ),
    (
        17,
        "b0b7b7a6fcb3bd66a9ae74c315226f7db3e292087773c22b2948eb6e9b351ab7",
    ),
];

#[allow(dead_code)]
fn assert_frozen(name: &str, actual: Vec<(i64, String)>, frozen: &[(i64, &str)]) {
    for (version, checksum) in frozen {
        let found = actual.iter().find(|(v, _)| v == version);
        assert_eq!(
            found.map(|(_, c)| c.as_str()),
            Some(*checksum),
            "{name} migration {version} changed: existing databases would refuse to start"
        );
    }
}

#[cfg(feature = "sqlite")]
#[test]
fn sqlite_migration_checksums_are_frozen() {
    let actual = memquira_storage::migrations::sqlite_migrations()
        .into_iter()
        .map(|m| (m.version, m.checksum))
        .collect();
    assert_frozen("sqlite", actual, SQLITE);
}

#[cfg(feature = "postgres")]
#[test]
fn postgres_migration_checksums_are_frozen() {
    let actual = memquira_storage::migrations::postgres_migrations()
        .into_iter()
        .map(|m| (m.version, m.checksum))
        .collect();
    assert_frozen("postgres", actual, POSTGRES);
}
