//! Opus receive-limit admission for unchanged shared-source forwarding.

/// The receiver cannot carry the shared Opus source without lowering it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("receiver audio bitrate limit cannot carry the shared Opus source")]
pub struct AudioLimitError;

/// Admits unchanged Opus forwarding. A receiver may not lower the shared
/// source or its siblings. An omitted preference uses codec defaults, not an
/// unrestricted rate. Rejected (port 0) and non-audio media impose no limit.
/// Invalid `maxaveragebitrate` values are ignored (RFC 7587).
pub fn check_audio_receive_limit(
    description: &str,
    source_ceiling: u32,
) -> Result<(), AudioLimitError> {
    let mut sections = Vec::new();
    for line in description.lines().map(str::trim_end) {
        if let Some(media) = line.strip_prefix("m=") {
            let mut fields = media.split_whitespace();
            let audio = fields.next() == Some("audio");
            let active = fields
                .next()
                .is_some_and(|port| port.split('/').next() != Some("0"));
            let formats: Vec<&str> = fields.skip(1).collect();
            sections.push((audio && active, formats, Vec::<&str>::new()));
        } else if let Some((_, _, attributes)) = sections.last_mut()
            && let Some(attribute) = line.strip_prefix("a=")
        {
            attributes.push(attribute);
        }
    }
    for (_, formats, attributes) in sections.iter().filter(|(admitted, ..)| *admitted) {
        for format in formats {
            if format.parse::<u8>().is_err() {
                continue;
            }
            let opus = attributes.iter().any(|attribute| {
                attribute
                    .strip_prefix("rtpmap:")
                    .and_then(|rest| rest.split_once(' '))
                    .is_some_and(|(payload, codec)| {
                        payload == *format
                            && codec
                                .split('/')
                                .next()
                                .is_some_and(|name| name.eq_ignore_ascii_case("opus"))
                    })
            });
            if !opus {
                continue;
            }
            let mut limit = 0u32;
            for attribute in attributes {
                let Some((payload, parameters)) = attribute
                    .strip_prefix("fmtp:")
                    .and_then(|rest| rest.split_once(' '))
                else {
                    continue;
                };
                if payload != *format {
                    continue;
                }
                for parameter in parameters.split(';') {
                    let Some((name, value)) = parameter.split_once('=') else {
                        continue;
                    };
                    if !name.trim().eq_ignore_ascii_case("maxaveragebitrate") {
                        continue;
                    }
                    let Ok(value) = value.trim().parse::<u32>() else {
                        continue;
                    };
                    if (6_000..=510_000).contains(&value) && (limit == 0 || value < limit) {
                        limit = value;
                    }
                }
            }
            if limit < source_ceiling {
                return Err(AudioLimitError);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_audio_rejects_only_an_insufficient_receiver() {
        for (parameter, allowed) in [
            ("stereo=1", false),
            ("stereo=1;maxaveragebitrate=510000", true),
            ("stereo=1;maxaveragebitrate=192000", false),
            ("maxaveragebitrate=320000", false),
            ("maxaveragebitrate=invalid", false),
            ("maxaveragebitrate=510001", false),
            ("maxaveragebitrate=510000;maxaveragebitrate=510001", true),
            ("maxaveragebitrate=510000;maxaveragebitrate=96000", false),
        ] {
            let answer = format!(
                "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\n\
                 m=audio 9 UDP/TLS/RTP/SAVPF 111\r\na=rtpmap:111 opus/48000/2\r\na=fmtp:111 {parameter}\r\n"
            );
            assert_eq!(
                check_audio_receive_limit(&answer, 510_000).is_ok(),
                allowed,
                "{parameter}"
            );
            let rejected = answer.replacen("m=audio 9", "m=audio 0", 1);
            assert!(
                check_audio_receive_limit(&rejected, 510_000).is_ok(),
                "{parameter}"
            );
        }
        let video = "v=0\r\nm=video 9 UDP/TLS/RTP/SAVPF 96\r\na=rtpmap:96 VP8/90000\r\n";
        assert!(check_audio_receive_limit(video, 510_000).is_ok());
    }
}
