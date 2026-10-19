//! The one-line pointer to `EXPERIENCE.md` in a Claude Code rules file
//! (`.claude/rules/agentdiet.md`, or `~/.claude/rules/agentdiet.md` at user scope). Claude Code
//! loads every rules file without `paths` frontmatter at launch (code.claude.com/docs/en/memory),
//! so the agent learns that `agentdiet reflect`'s lessons exist without anyone editing CLAUDE.md.
//!
//! The pointer is a single line ending in [`MARKER`]; patching appends or refreshes that line and
//! unpatching removes only it, so anything else a user writes in the file is kept.

use super::json_patch::PatchError;

/// Ends every pointer line agentdiet writes, so an older wording is still recognised as ours.
pub const MARKER: &str = "<!-- agentdiet:experience-pointer -->";

/// The pointer line, without its line break.
pub fn line() -> String {
    format!(
        "Before running build, test, lint or install commands, read `EXPERIENCE.md` at the project \
         root if it exists: repo-specific command recipes and pitfalls learned by `agentdiet reflect`. \
         {MARKER}"
    )
}

fn is_ours(l: &str) -> bool {
    l.trim_end().ends_with(MARKER)
}

/// Ensure the current pointer is present. `Ok(None)` means already up to date.
pub fn patch(current: Option<&str>) -> Result<Option<String>, PatchError> {
    let want = line();
    let Some(text) = current else {
        return Ok(Some(format!("{want}\n")));
    };
    if text.lines().any(|l| l == want) && text.lines().filter(|l| is_ours(l)).count() == 1 {
        return Ok(None);
    }
    let mut out: Vec<&str> = text.lines().filter(|l| !is_ours(l)).collect();
    while out.last().is_some_and(|l| l.trim().is_empty()) {
        out.pop();
    }
    let mut text = out.join("\n");
    if !text.is_empty() {
        text.push_str("\n\n");
    }
    text.push_str(&want);
    text.push('\n');
    Ok(Some(text))
}

/// Remove the pointer line. `Ok(None)` means there was none.
pub fn unpatch(text: &str) -> Result<Option<String>, PatchError> {
    if !is_present(text) {
        return Ok(None);
    }
    let kept: Vec<&str> = text.lines().filter(|l| !is_ours(l)).collect();
    let mut out = kept.join("\n");
    let trimmed = out.trim_end().len();
    out.truncate(trimmed);
    if !out.is_empty() {
        out.push('\n');
    }
    Ok(Some(out))
}

pub fn is_present(text: &str) -> bool {
    text.lines().any(is_ours)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_refreshes_and_removes_only_its_own_line() {
        let created = patch(None).unwrap().unwrap();
        assert_eq!(created, format!("{}\n", line()));
        assert_eq!(created.lines().count(), 1, "a one-line pointer");
        assert_eq!(patch(Some(&created)).unwrap(), None, "idempotent");

        let mine = "# My rules\n\nAlways use pnpm.\n";
        let added = patch(Some(mine)).unwrap().unwrap();
        assert!(added.starts_with(mine) && added.ends_with(&format!("{}\n", line())));
        assert_eq!(unpatch(&added).unwrap().unwrap(), mine);

        let old = format!("# My rules\nRead EXPERIENCE.md. {MARKER}\n");
        let refreshed = patch(Some(&old)).unwrap().unwrap();
        assert_eq!(refreshed.matches(MARKER).count(), 1);
        assert!(refreshed.contains(&line()));

        assert_eq!(unpatch(&created).unwrap().unwrap(), "");
        assert_eq!(unpatch(mine).unwrap(), None);
    }
}
