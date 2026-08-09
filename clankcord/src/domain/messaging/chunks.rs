pub(crate) const MESSAGE_CHUNK_LIMIT: usize = 1800;

#[derive(Debug, Clone)]
enum MessageChunkUnit {
    Plain(String),
    Code {
        opening: String,
        body: Vec<String>,
        closing: Option<String>,
    },
}

#[derive(Debug, Clone)]
struct FenceMarker {
    character: char,
    width: usize,
}

pub fn split_message_chunks(content: &str, limit: usize) -> Vec<String> {
    let normalized = content.trim();
    if normalized.is_empty() {
        return Vec::new();
    }
    if limit == 0 {
        return Vec::new();
    }
    let mut chunks = Vec::new();
    let mut current = String::new();
    for unit in message_chunk_units(normalized) {
        match unit {
            MessageChunkUnit::Plain(block) => {
                append_plain_block(&mut chunks, &mut current, &block, limit);
            }
            MessageChunkUnit::Code {
                opening,
                body,
                closing,
            } => {
                let code_block = code_block_text(&opening, &body, closing.as_deref());
                if closing.is_some() && code_block.len() <= limit {
                    append_plain_block(&mut chunks, &mut current, &code_block, limit);
                } else {
                    push_current_chunk(&mut chunks, &mut current);
                    chunks.extend(split_code_block(&opening, &body, limit));
                }
            }
        }
    }
    push_current_chunk(&mut chunks, &mut current);
    chunks
}

fn message_chunk_units(content: &str) -> Vec<MessageChunkUnit> {
    let lines = content.lines().collect::<Vec<_>>();
    let mut units = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        if line.trim().is_empty() {
            index += 1;
            continue;
        }
        if let Some(marker) = fence_marker(line) {
            let opening = line.to_string();
            index += 1;
            let mut body = Vec::new();
            let mut closing = None;
            while index < lines.len() {
                let candidate = lines[index];
                if is_closing_fence(candidate, &marker) {
                    closing = Some(candidate.to_string());
                    index += 1;
                    break;
                }
                body.push(candidate.to_string());
                index += 1;
            }
            units.push(MessageChunkUnit::Code {
                opening,
                body,
                closing,
            });
            continue;
        }
        if is_list_marker(line) {
            let mut block = Vec::new();
            while index < lines.len() {
                let candidate = lines[index];
                if candidate.trim().is_empty() || fence_marker(candidate).is_some() {
                    break;
                }
                block.push(candidate);
                index += 1;
            }
            units.push(MessageChunkUnit::Plain(block.join("\n")));
            continue;
        }
        let mut block = Vec::new();
        while index < lines.len() {
            let candidate = lines[index];
            if candidate.trim().is_empty()
                || fence_marker(candidate).is_some()
                || is_list_marker(candidate)
            {
                break;
            }
            block.push(candidate);
            index += 1;
        }
        units.push(MessageChunkUnit::Plain(block.join("\n")));
    }
    units
}

fn append_plain_block(chunks: &mut Vec<String>, current: &mut String, block: &str, limit: usize) {
    let block = block.trim();
    if block.is_empty() {
        return;
    }
    let separator = if current.is_empty() { "" } else { "\n\n" };
    if current.len() + separator.len() + block.len() <= limit {
        current.push_str(separator);
        current.push_str(block);
        return;
    }
    push_current_chunk(chunks, current);
    if block.len() <= limit {
        current.push_str(block);
        return;
    }
    chunks.extend(split_plain_block(block, limit));
}

fn split_plain_block(block: &str, limit: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    for line in block.lines() {
        if line.len() > limit {
            push_current_chunk(&mut chunks, &mut current);
            chunks.extend(split_long_line(line, limit));
            continue;
        }
        let separator = if current.is_empty() { "" } else { "\n" };
        if current.len() + separator.len() + line.len() > limit {
            push_current_chunk(&mut chunks, &mut current);
        }
        if !current.is_empty() {
            current.push('\n');
        }
        current.push_str(line);
    }
    push_current_chunk(&mut chunks, &mut current);
    chunks
}

fn split_long_line(line: &str, limit: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    for word in line.split_inclusive(' ') {
        if word.len() > limit {
            push_current_chunk(&mut chunks, &mut current);
            chunks.extend(split_at_char_boundaries(word.trim_end(), limit));
            continue;
        }
        if current.len() + word.len() > limit {
            push_current_chunk(&mut chunks, &mut current);
        }
        current.push_str(word);
    }
    push_current_chunk(&mut chunks, &mut current);
    chunks
}

fn split_at_char_boundaries(value: &str, limit: usize) -> Vec<String> {
    if limit == 0 {
        return Vec::new();
    }
    let mut chunks = Vec::new();
    let mut current = String::new();
    for character in value.chars() {
        let length = character.len_utf8();
        if !current.is_empty() && current.len() + length > limit {
            chunks.push(current);
            current = String::new();
        }
        current.push(character);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

fn split_code_block(opening: &str, body: &[String], limit: usize) -> Vec<String> {
    let closing = closing_fence_for(opening);
    let overhead = opening.len() + closing.len() + 2;
    if limit <= overhead {
        return split_plain_block(&code_block_text(opening, body, Some(&closing)), limit);
    }
    let body_limit = limit - overhead;
    let mut chunks = Vec::new();
    let mut current = String::new();
    for line in body {
        if line.len() > body_limit {
            push_wrapped_code_chunk(&mut chunks, opening, &closing, &mut current);
            for part in split_long_line(line, body_limit) {
                chunks.push(wrap_code_chunk(opening, &closing, &part));
            }
            continue;
        }
        let separator = if current.is_empty() { "" } else { "\n" };
        if current.len() + separator.len() + line.len() > body_limit {
            push_wrapped_code_chunk(&mut chunks, opening, &closing, &mut current);
        }
        if !current.is_empty() {
            current.push('\n');
        }
        current.push_str(line);
    }
    push_wrapped_code_chunk(&mut chunks, opening, &closing, &mut current);
    if chunks.is_empty() {
        chunks.push(wrap_code_chunk(opening, &closing, ""));
    }
    chunks
}

fn push_wrapped_code_chunk(
    chunks: &mut Vec<String>,
    opening: &str,
    closing: &str,
    current: &mut String,
) {
    if current.is_empty() {
        return;
    }
    chunks.push(wrap_code_chunk(opening, closing, current));
    current.clear();
}

fn wrap_code_chunk(opening: &str, closing: &str, body: &str) -> String {
    if body.is_empty() {
        format!("{opening}\n{closing}")
    } else {
        format!("{opening}\n{body}\n{closing}")
    }
}

fn code_block_text(opening: &str, body: &[String], closing: Option<&str>) -> String {
    let mut lines = Vec::with_capacity(body.len() + usize::from(closing.is_some()) + 1);
    lines.push(opening.to_string());
    lines.extend(body.iter().cloned());
    if let Some(closing) = closing {
        lines.push(closing.to_string());
    }
    lines.join("\n")
}

fn push_current_chunk(chunks: &mut Vec<String>, current: &mut String) {
    let chunk = current.trim_end();
    if !chunk.trim().is_empty() {
        chunks.push(chunk.to_string());
    }
    current.clear();
}

fn fence_marker(line: &str) -> Option<FenceMarker> {
    let trimmed = line.trim_start();
    let character = trimmed.chars().next()?;
    if character != '`' && character != '~' {
        return None;
    }
    let width = trimmed
        .chars()
        .take_while(|candidate| *candidate == character)
        .count();
    (width >= 3).then_some(FenceMarker { character, width })
}

fn is_closing_fence(line: &str, marker: &FenceMarker) -> bool {
    let trimmed = line.trim_start();
    let width = trimmed
        .chars()
        .take_while(|candidate| *candidate == marker.character)
        .count();
    width >= marker.width && trimmed[width..].trim().is_empty()
}

fn closing_fence_for(opening: &str) -> String {
    let marker = fence_marker(opening).expect("opening line is a fence");
    std::iter::repeat_n(marker.character, marker.width).collect()
}

fn is_list_marker(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("+ ") {
        return true;
    }
    let digits = trimmed
        .chars()
        .take_while(|value| value.is_ascii_digit())
        .count();
    if digits == 0 {
        return false;
    }
    let rest = &trimmed[digits..];
    rest.starts_with(". ") || rest.starts_with(") ")
}
