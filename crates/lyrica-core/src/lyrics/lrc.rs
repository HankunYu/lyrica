use std::time::Duration;

use anyhow::Result;
use regex::Regex;

use super::{Lyrics, LyricsLine, LyricsMetadata, LyricsSource};

/// Parse an LRC format string into a Lyrics struct.
///
/// Supports:
/// - `[mm:ss.xx]` and `[mm:ss.xxx]` time tags
/// - Multiple time tags per line: `[00:12.34][00:45.67]text`
/// - Metadata tags: `[ti:Title]`, `[ar:Artist]`, `[al:Album]`, `[offset:+/-ms]`
pub fn parse(input: &str) -> Result<Lyrics> {
    let time_re = Regex::new(r"\[(\d{2}):(\d{2})\.(\d{2,3})\]")?;
    let meta_re = Regex::new(r"^\[(\w+):(.+)\]$")?;

    let mut lines: Vec<LyricsLine> = Vec::new();
    let mut title: Option<String> = None;
    let mut artist: Option<String> = None;
    let mut album: Option<String> = None;
    let mut offset_ms: i64 = 0;

    for raw_line in input.lines() {
        let raw_line = raw_line.trim();
        if raw_line.is_empty() {
            continue;
        }

        // Try metadata tag first (lines with only one tag and no time format).
        if let Some(caps) = meta_re.captures(raw_line) {
            let key = caps.get(1).unwrap().as_str().to_lowercase();
            let value = caps.get(2).unwrap().as_str().trim().to_string();
            match key.as_str() {
                "ti" => title = Some(value),
                "ar" => artist = Some(value),
                "al" => album = Some(value),
                "offset" => {
                    offset_ms = value.parse::<i64>().unwrap_or(0);
                }
                _ => {} // Ignore unknown metadata tags.
            }
            continue;
        }

        // Collect all time tags.
        let timestamps: Vec<Duration> = time_re
            .captures_iter(raw_line)
            .filter_map(|caps| {
                let min: u64 = caps.get(1)?.as_str().parse().ok()?;
                let sec: u64 = caps.get(2)?.as_str().parse().ok()?;
                let frac_str = caps.get(3)?.as_str();
                let millis: u64 = if frac_str.len() == 2 {
                    frac_str.parse::<u64>().ok()? * 10
                } else {
                    frac_str.parse::<u64>().ok()?
                };
                Some(Duration::from_millis(min * 60_000 + sec * 1000 + millis))
            })
            .collect();

        if timestamps.is_empty() {
            continue;
        }

        // Extract the text content after all time tags.
        let content = time_re.replace_all(raw_line, "").trim().to_string();

        // One line per timestamp (handles multiple tags like [00:12.34][00:45.67]text).
        for ts in timestamps {
            lines.push(LyricsLine {
                position: ts,
                content: content.clone(),
                translation: None,
                word_timestamps: None,
            });
        }
    }

    // Sort by position.
    lines.sort_by_key(|l| l.position);

    Ok(Lyrics {
        lines,
        metadata: LyricsMetadata {
            title,
            artist,
            album,
            source: LyricsSource::Unknown,
            quality: 0.0,
        },
        offset_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_basic_lrc() {
        let input = r#"[ti:Test Song]
[ar:Test Artist]
[al:Test Album]
[00:12.34]First line
[00:15.67]Second line
[00:20.00]Third line"#;

        let lyrics = parse(input).unwrap();
        assert_eq!(lyrics.metadata.title.as_deref(), Some("Test Song"));
        assert_eq!(lyrics.metadata.artist.as_deref(), Some("Test Artist"));
        assert_eq!(lyrics.lines.len(), 3);
        assert_eq!(lyrics.lines[0].content, "First line");
        assert_eq!(
            lyrics.lines[0].position,
            Duration::from_millis(12 * 1000 + 340)
        );
        assert_eq!(lyrics.lines[1].content, "Second line");
        assert_eq!(lyrics.lines[2].content, "Third line");
    }

    #[test]
    fn test_parse_multiple_timestamps() {
        let input = "[00:12.34][01:12.34]Repeated line";
        let lyrics = parse(input).unwrap();
        assert_eq!(lyrics.lines.len(), 2);
        assert_eq!(lyrics.lines[0].content, "Repeated line");
        assert_eq!(lyrics.lines[1].content, "Repeated line");
        assert!(lyrics.lines[0].position < lyrics.lines[1].position);
    }

    #[test]
    fn test_parse_three_digit_millis() {
        let input = "[00:12.345]Line with 3-digit ms";
        let lyrics = parse(input).unwrap();
        assert_eq!(
            lyrics.lines[0].position,
            Duration::from_millis(12 * 1000 + 345)
        );
    }

    #[test]
    fn test_line_at() {
        let input = "[00:05.00]Line 1\n[00:10.00]Line 2\n[00:15.00]Line 3";
        let lyrics = parse(input).unwrap();

        // Before first line.
        let (cur, next) = lyrics.line_at(Duration::from_secs(3));
        assert_eq!(cur, None);
        assert_eq!(next, Some(0));

        // At first line.
        let (cur, next) = lyrics.line_at(Duration::from_secs(5));
        assert_eq!(cur, Some(0));
        assert_eq!(next, Some(1));

        // Between lines.
        let (cur, next) = lyrics.line_at(Duration::from_secs(12));
        assert_eq!(cur, Some(1));
        assert_eq!(next, Some(2));

        // After last line.
        let (cur, next) = lyrics.line_at(Duration::from_secs(20));
        assert_eq!(cur, Some(2));
        assert_eq!(next, None);
    }
}
