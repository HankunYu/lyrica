use std::time::Duration;

use anyhow::Result;
use regex::Regex;

use super::lrc;
use super::WordTimestamp;

/// Parse an LRCX format string into Lyrics.
///
/// LRCX extends LRC with inline word-level time tags:
///   `[00:12.34]<00:12.34>First <00:12.80>line <00:13.20>of`
///
/// And translation lines tagged with `[tr]` or `[tr:lang]`:
///   `[00:12.34]Original line`
///   `[00:12.34][tr:zh]translated line`
pub fn parse(input: &str) -> Result<super::Lyrics> {
    // First pass: use standard LRC parser.
    let mut lyrics = lrc::parse(input)?;

    let word_time_re = Regex::new(r"<(\d{2}):(\d{2})\.(\d{2,3})>")?;
    let tr_line_re = Regex::new(r"\[(\d{2}):(\d{2})\.(\d{2,3})\]\[tr(?::(\w+))?\](.+)")?;

    // Second pass: extract word timestamps and translations.
    let mut translations: Vec<(Duration, String)> = Vec::new();

    for raw_line in input.lines() {
        let raw_line = raw_line.trim();
        if raw_line.is_empty() {
            continue;
        }

        // Check for translation lines.
        if let Some(caps) = tr_line_re.captures(raw_line) {
            let min: u64 = caps.get(1).unwrap().as_str().parse()?;
            let sec: u64 = caps.get(2).unwrap().as_str().parse()?;
            let frac_str = caps.get(3).unwrap().as_str();
            let millis: u64 = if frac_str.len() == 2 {
                frac_str.parse::<u64>()? * 10
            } else {
                frac_str.parse()?
            };
            let pos = Duration::from_millis(min * 60_000 + sec * 1000 + millis);
            let text = caps.get(5).unwrap().as_str().to_string();
            translations.push((pos, text));
            continue;
        }
    }

    // Apply translations to matching lines.
    for (pos, text) in &translations {
        if let Some(line) = lyrics.lines.iter_mut().find(|l| l.position == *pos) {
            line.translation = Some(text.clone());
        }
    }

    // Third pass: extract word timestamps from line content.
    for line in &mut lyrics.lines {
        if !word_time_re.is_match(&line.content) {
            continue;
        }

        let mut words = Vec::new();
        let parts: Vec<&str> = word_time_re.split(&line.content).collect();
        let timestamps: Vec<Duration> = word_time_re
            .captures_iter(&line.content)
            .filter_map(|caps| {
                let min: u64 = caps.get(1)?.as_str().parse().ok()?;
                let sec: u64 = caps.get(2)?.as_str().parse().ok()?;
                let frac_str = caps.get(3)?.as_str();
                let millis: u64 = if frac_str.len() == 2 {
                    frac_str.parse::<u64>().ok()? * 10
                } else {
                    frac_str.parse().ok()?
                };
                Some(Duration::from_millis(min * 60_000 + sec * 1000 + millis))
            })
            .collect();

        // Build word timestamps.
        // parts[0] is text before first tag (usually empty), parts[i+1] is text after tag[i].
        for (i, ts) in timestamps.iter().enumerate() {
            let word_text = parts.get(i + 1).unwrap_or(&"").trim().to_string();
            if word_text.is_empty() {
                continue;
            }
            let duration = timestamps
                .get(i + 1)
                .map(|next| next.saturating_sub(*ts))
                .unwrap_or(Duration::from_millis(500)); // Default duration for last word.
            let offset = ts.saturating_sub(line.position);
            words.push(WordTimestamp {
                offset,
                duration,
                word: word_text,
            });
        }

        if !words.is_empty() {
            // Rebuild clean content without time tags.
            line.content = word_time_re.replace_all(&line.content, "").trim().to_string();
            line.word_timestamps = Some(words);
        }
    }

    // Remove translation-only lines from the main lines list.
    // (Translation lines that matched an existing line position are already merged.)
    let tr_positions: std::collections::HashSet<u128> = translations
        .iter()
        .map(|(pos, _)| pos.as_millis())
        .collect();
    lyrics.lines.retain(|l| {
        // Keep lines that have content or are not pure translation duplicates.
        !l.content.is_empty() || !tr_positions.contains(&l.position.as_millis())
    });

    Ok(lyrics)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_word_timestamps() {
        let input = "[00:12.00]<00:12.00>Hello <00:12.50>world <00:13.00>test";
        let lyrics = parse(input).unwrap();
        assert_eq!(lyrics.lines.len(), 1);
        let line = &lyrics.lines[0];
        assert_eq!(line.content, "Hello world test");
        let wt = line.word_timestamps.as_ref().unwrap();
        assert_eq!(wt.len(), 3);
        assert_eq!(wt[0].word, "Hello");
        assert_eq!(wt[1].word, "world");
        assert_eq!(wt[2].word, "test");
    }

    #[test]
    fn test_parse_translations() {
        let input = "[00:12.00]Original line\n[00:12.00][tr:zh]translated";
        let lyrics = parse(input).unwrap();
        // Should have one line with translation.
        let line = lyrics.lines.iter().find(|l| l.content == "Original line");
        assert!(line.is_some());
        assert_eq!(
            line.unwrap().translation.as_deref(),
            Some("translated")
        );
    }
}
