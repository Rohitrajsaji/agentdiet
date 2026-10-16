//! A tiny line diff for `--dry-run` output.

/// Beyond this many LCS cells the middle section is shown as a plain
/// remove-all / add-all block instead of a minimal diff.
const MAX_LCS_CELLS: usize = 4_000_000;

/// Render the changes between `old` and `new` as `-`/`+` lines, with a header.
/// Unchanged lines are omitted; the file's unchanged head/tail are summarised.
pub fn render(path: &str, old: &str, new: &str) -> String {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();

    let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suffix = a[prefix..]
        .iter()
        .rev()
        .zip(b[prefix..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let a_mid = &a[prefix..a.len() - suffix];
    let b_mid = &b[prefix..b.len() - suffix];

    let mut out = format!("--- {path}\n+++ {path} (agentdiet)\n");
    if prefix > 0 {
        out.push_str(&format!("  … {prefix} unchanged line(s)\n"));
    }
    for (op, line) in diff_middle(a_mid, b_mid) {
        out.push(op);
        out.push(' ');
        out.push_str(line);
        out.push('\n');
    }
    if suffix > 0 {
        out.push_str(&format!("  … {suffix} unchanged line(s)\n"));
    }
    out
}

fn diff_middle<'a>(a: &[&'a str], b: &[&'a str]) -> Vec<(char, &'a str)> {
    if a.is_empty() || b.is_empty() || a.len().saturating_mul(b.len()) > MAX_LCS_CELLS {
        return a
            .iter()
            .map(|l| ('-', *l))
            .chain(b.iter().map(|l| ('+', *l)))
            .collect();
    }
    // lcs[i][j] = LCS length of a[i..] and b[j..]
    let width = b.len() + 1;
    let mut lcs = vec![0u32; (a.len() + 1) * width];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i * width + j] = if a[i] == b[j] {
                lcs[(i + 1) * width + j + 1] + 1
            } else {
                lcs[(i + 1) * width + j].max(lcs[i * width + j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::new();
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            out.push((' ', a[i]));
            i += 1;
            j += 1;
        } else if lcs[(i + 1) * width + j] >= lcs[i * width + j + 1] {
            out.push(('-', a[i]));
            i += 1;
        } else {
            out.push(('+', b[j]));
            j += 1;
        }
    }
    out.extend(a[i..].iter().map(|l| ('-', *l)));
    out.extend(b[j..].iter().map(|l| ('+', *l)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shows_additions_and_removals() {
        let out = render("f", "a\nb\nc\n", "a\nx\nc\n");
        assert!(out.contains("- b"));
        assert!(out.contains("+ x"));
        assert!(out.contains("1 unchanged"));
        assert!(!out.contains("- a"));
    }

    #[test]
    fn new_file_is_all_additions() {
        let out = render("f", "", "one\ntwo\n");
        assert!(out.contains("+ one"));
        assert!(out.contains("+ two"));
    }

    #[test]
    fn interleaved_changes_keep_context_lines() {
        let out = render("f", "a\nb\nc\nd\n", "a\nB\nc\nD\n");
        assert!(out.contains("  c"));
        assert!(out.contains("- b") && out.contains("+ B"));
        assert!(out.contains("- d") && out.contains("+ D"));
    }
}
