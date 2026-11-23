//! `PRIVACY.md` keeps a copy of the outbound list. `CH_BLESS=1` rewrites the copy.

use std::path::PathBuf;

const START: &str = "<!-- outbound:start -->\n";
const END: &str = "<!-- outbound:end -->";

#[test]
fn privacy_md_matches_the_outbound_list() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../PRIVACY.md");
    let text = std::fs::read_to_string(&path)
        .expect("read PRIVACY.md")
        .replace("\r\n", "\n");
    let start = text
        .find(START)
        .expect("PRIVACY.md lost its outbound:start marker")
        + START.len();
    let end = text
        .find(END)
        .expect("PRIVACY.md lost its outbound:end marker");
    let want = ch_core::outbound_markdown();
    if text[start..end] == want {
        return;
    }
    if std::env::var_os("CH_BLESS").is_some() {
        let fresh = format!("{}{want}{}", &text[..start], &text[end..]);
        std::fs::write(&path, fresh).expect("write PRIVACY.md");
        return;
    }
    let missing: Vec<&str> = ch_core::OUTBOUND
        .iter()
        .filter(|row| !text[start..end].contains(row.what))
        .map(|row| row.id)
        .collect();
    panic!(
        "PRIVACY.md is out of date (rows missing or changed: {missing:?}). Run \
         `CH_BLESS=1 cargo test -p ch-core --test privacy` and commit the result."
    );
}
