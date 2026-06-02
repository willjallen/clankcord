use clankcord::runtime::split_message_chunks;

#[test]
fn markdown_chunks_balance_split_code_fences() {
    let body = (0..24)
        .map(|index| format!("let value_{index} = compute_value({index});"))
        .collect::<Vec<_>>()
        .join("\n");
    let content = format!("Before\n\n```rust\n{body}\n```\n\nAfter");
    let chunks = split_message_chunks(&content, 180);

    assert!(chunks.len() > 2);
    assert!(chunks.iter().all(|chunk| chunk.len() <= 180));
    for chunk in &chunks {
        assert_balanced_code_fences(chunk);
    }
    assert!(
        chunks
            .iter()
            .filter(|chunk| chunk.starts_with("```rust"))
            .count()
            > 1
    );
    assert!(chunks.iter().any(|chunk| chunk.contains("value_0")));
    assert!(chunks.iter().any(|chunk| chunk.contains("value_23")));
}

#[test]
fn markdown_chunks_close_unclosed_code_fence() {
    let chunks = split_message_chunks("```text\nan unfinished code block", 80);

    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0], "```text\nan unfinished code block\n```");
    assert_balanced_code_fences(&chunks[0]);
}

#[test]
fn markdown_chunks_keep_list_blocks_together_when_possible() {
    let content = concat!(
        "Intro paragraph that fills some room.\n\n",
        "- first item\n",
        "  continuation detail\n",
        "- second item\n",
        "  more detail\n\n",
        "Closing paragraph."
    );
    let chunks = split_message_chunks(content, 120);

    assert!(chunks.iter().all(|chunk| chunk.len() <= 120));
    assert!(chunks.iter().any(|chunk| {
        chunk.contains("- first item\n  continuation detail\n- second item\n  more detail")
    }));
}

#[test]
fn markdown_chunks_split_long_list_item_without_orphaning_marker() {
    let content = format!("- {}", "longword ".repeat(80));
    let chunks = split_message_chunks(&content, 100);

    assert!(chunks.len() > 1);
    assert!(chunks.iter().all(|chunk| chunk.len() <= 100));
    assert_ne!(chunks[0].trim(), "-");
    assert!(chunks[0].starts_with("- longword"));
}

#[test]
fn markdown_chunks_split_paragraphs_under_limit() {
    let content = ["alpha beta gamma delta epsilon"; 20].join("\n");
    let chunks = split_message_chunks(&content, 90);

    assert!(chunks.len() > 1);
    assert!(chunks.iter().all(|chunk| chunk.len() <= 90));
    assert!(chunks[0].contains("alpha beta"));
}

fn assert_balanced_code_fences(chunk: &str) {
    let mut open = false;
    for line in chunk.lines() {
        if line.trim_start().starts_with("```") {
            open = !open;
        }
    }
    assert!(!open, "{chunk}");
}
