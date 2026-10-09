//! The always-on scanner against CommonMark's rules for what hides an
//! `@import`: code blocks and fences, and how paths are reported.
use super::clean_path;
use crate::*;

fn imports(text: &str) -> Vec<String> {
    claude_md_imports(text)
}

// ── indented lines: code block or paragraph continuation ────────────────

#[test]
fn indented_line_after_a_paragraph_line_is_a_continuation() {
    // CommonMark 4.4: an indented code block cannot interrupt a paragraph,
    // so the second line is paragraph text and its import counts.
    assert_eq!(imports("some text\n    @./cont.md\n"), vec!["./cont.md"]);
    assert_eq!(imports("some text\n\t@./tab.md\n"), vec!["./tab.md"]);
    // Still a continuation on the second and third indented line.
    assert_eq!(
        imports("some text\n    @./a.md\n    @./b.md\n"),
        vec!["./a.md", "./b.md"]
    );
}

#[test]
fn indented_line_after_a_blank_line_is_code() {
    assert!(imports("some text\n\n    @./code.md\n").is_empty());
    assert!(imports("some text\n\n\t@./code.md\n").is_empty());
    // Code continues through its own lines and through blank lines in it.
    assert!(imports("\n    @./a.md\n    @./b.md\n\n    @./c.md\n").is_empty());
    // At the very start of the file.
    assert!(imports("    @./first.md\n").is_empty());
}

#[test]
fn indented_line_after_a_heading_rule_or_underline_is_code() {
    assert!(imports("# Title\n    @./code.md\n").is_empty());
    assert!(imports("###### Title ##\n    @./code.md\n").is_empty());
    assert!(imports("---\n    @./code.md\n").is_empty());
    assert!(imports("* * *\n    @./code.md\n").is_empty());
    assert!(imports("Title\n=====\n    @./code.md\n").is_empty());
    // `#hashtag` is not a heading, so it is paragraph text.
    assert_eq!(imports("#hashtag\n    @./cont.md\n"), vec!["./cont.md"]);
}

#[test]
fn indented_line_after_a_fence_is_code_and_a_fence_ends_a_paragraph() {
    assert!(imports("```\nx\n```\n    @./code.md\n").is_empty());
    // A fence interrupts a paragraph; the indented line after it is code.
    assert!(imports("text\n```\n```\n    @./code.md\n").is_empty());
}

// ── closing fences ──────────────────────────────────────────────────────

#[test]
fn closing_fence_indented_four_columns_does_not_close() {
    // The four-space line is content of the fence, so the import after it
    // is still inside the fence and does not count.
    assert!(imports("```\n    ```\n@./inside.md\n```\n").is_empty());
    assert!(imports("```\n\t```\n@./inside.md\n```\n").is_empty());
}

#[test]
fn closing_fence_indented_up_to_three_columns_closes() {
    assert_eq!(imports("```\n   ```\n@./out.md\n"), vec!["./out.md"]);
    assert_eq!(imports("```\n ```\n@./out.md\n"), vec!["./out.md"]);
}

#[test]
fn closing_fence_must_be_as_long_and_the_same_character() {
    // A shorter run, or the other character, does not close.
    assert!(imports("````\n```\n@./inside.md\n````\n").is_empty());
    assert!(imports("```\n~~~\n@./inside.md\n```\n").is_empty());
    assert!(imports("~~~\n```\n@./inside.md\n~~~\n").is_empty());
    // A longer run closes, and so does an equal one.
    assert_eq!(imports("```\n`````\n@./out.md\n"), vec!["./out.md"]);
    // Text after the run means it is not a closing fence.
    assert!(imports("```\n``` x\n@./inside.md\n```\n").is_empty());
}

// ── reported paths ──────────────────────────────────────────────────────

#[test]
fn always_on_files_lists_cleaned_paths() {
    let root = std::env::temp_dir().join(format!("csr-scan-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("a/b")).unwrap();
    std::fs::write(root.join("a/CLAUDE.md"), "@./b/./inc.md\n@./b//dup.md\n").unwrap();
    std::fs::write(root.join("a/b/inc.md"), "inc").unwrap();
    std::fs::write(root.join("a/b/dup.md"), "dup").unwrap();

    // The project directory itself is written with a `.` segment and a
    // doubled slash.
    let project = format!("{}/./a//", root.display());
    let on = always_on(&project, "");
    let _ = std::fs::remove_dir_all(&root);

    let listed: Vec<&str> = on
        .files
        .iter()
        .map(|(p, _)| p.as_str())
        .filter(|p| p.contains(root.to_str().unwrap()))
        .collect();
    assert_eq!(listed.len(), 3, "{listed:?}");
    for p in &listed {
        assert!(!p.contains("/./"), "{p}");
        assert!(!p.contains("//"), "{p}");
    }
    assert!(listed[0].ends_with("/a/CLAUDE.md"), "{listed:?}");
    assert!(listed[1].ends_with("/a/b/inc.md"), "{listed:?}");
    assert!(listed[2].ends_with("/a/b/dup.md"), "{listed:?}");
}

#[test]
fn clean_path_drops_current_dir_segments_and_keeps_parent_segments() {
    let p = std::path::Path::new("/x/./y//z/../w/./f.md");
    assert_eq!(clean_path(p), "/x/y/z/../w/f.md");
    assert_eq!(clean_path(std::path::Path::new("./f.md")), "f.md");
}
