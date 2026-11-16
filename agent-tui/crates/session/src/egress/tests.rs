use agent_core::config::Osc52;

use super::Egress;
use super::classify::{self, Reading};
use super::filter::Filtered;
use super::parser::{Parser, Sink};
use super::table::{
    Audience, Class, Mode, ModeQuery, Palette, Policy, Query, Recipient, Refusal, Tally,
    Terminator, Verdict,
};

fn policy(clipboard: Osc52) -> Policy {
    Policy { clipboard }
}

/// What a stream came to, whatever the chunks it arrived in.
#[derive(Debug, Default, PartialEq, Eq)]
struct Outcome {
    owner: Vec<u8>,
    watchers: Vec<u8>,
    queries: Vec<Query>,
    refused: Tally,
}

fn run(policy: Policy, chunks: &[&[u8]]) -> Outcome {
    let mut egress = Egress::new(policy);
    let mut outcome = Outcome::default();
    for chunk in chunks {
        let Filtered {
            owner,
            watchers,
            asked,
            refused,
        } = egress.feed(chunk);
        outcome.owner.extend(owner);
        outcome.watchers.extend(watchers);
        outcome
            .queries
            .extend(asked.into_iter().map(|asked| asked.query));
        outcome.refused.merge(refused);
    }
    outcome
}

fn tally(refusals: &[Refusal]) -> Tally {
    let mut tally = Tally::default();
    for refusal in refusals {
        tally.add(*refusal);
    }
    tally
}

#[derive(Debug)]
struct Case {
    name: &'static str,
    input: &'static [u8],
    owner: &'static [u8],
    watchers: &'static [u8],
    queries: Vec<Query>,
    refused: Vec<Refusal>,
    class: Option<Class>,
}

fn forwarded(class: Class, input: &'static [u8]) -> Case {
    Case {
        name: "forwarded",
        input,
        owner: input,
        watchers: input,
        queries: Vec::new(),
        refused: Vec::new(),
        class: Some(class),
    }
}

/// A mode list with modes the host doesn't know, of which only the listed
/// ones are forwarded.
fn trimmed(input: &'static [u8], kept: &'static [u8]) -> Case {
    Case {
        name: "trimmed",
        owner: kept,
        watchers: kept,
        refused: vec![Refusal::UnlistedMode],
        class: None,
        ..forwarded(Class::Modes, input)
    }
}

fn plain(input: &'static [u8]) -> Case {
    forwarded(Class::Text, input)
}

fn owner_only(class: Class, input: &'static [u8]) -> Case {
    Case {
        name: "owner only",
        watchers: b"",
        ..forwarded(class, input)
    }
}

fn dropped(refusal: Refusal, input: &'static [u8]) -> Case {
    Case {
        name: "dropped",
        input,
        owner: b"",
        watchers: b"",
        queries: Vec::new(),
        refused: vec![refusal],
        class: Some(Class::Refused(refusal)),
    }
}

fn asks(query: Query, input: &'static [u8]) -> Case {
    Case {
        name: "asks",
        input,
        owner: b"",
        watchers: b"",
        queries: vec![query],
        refused: Vec::new(),
        class: Some(Class::Answered(query)),
    }
}

fn mode_cases() -> Vec<Case> {
    Mode::ALL
        .into_iter()
        .flat_map(|mode| {
            let leaked = |direction: char| -> &'static [u8] {
                format!("\x1b[?{}{direction}", mode.number())
                    .leak()
                    .as_bytes()
            };
            [
                forwarded(Class::Modes, leaked('h')),
                forwarded(Class::Modes, leaked('l')),
            ]
        })
        .collect()
}

const OSC_END: &[&str] = &["\x07", "\x1b\\"];

fn osc_cases() -> Vec<Case> {
    let mut cases = Vec::new();
    for end in OSC_END {
        let titled = |class: Class, body: &str| -> Case {
            owner_only(class, format!("\x1b]{body}{end}").leak().as_bytes())
        };
        let link = |body: &str| -> Case {
            forwarded(
                Class::Hyperlink,
                format!("\x1b]8;;{body}{end}").leak().as_bytes(),
            )
        };
        cases.extend([
            titled(Class::Title, "0;a title"),
            titled(Class::Title, "1;an icon"),
            titled(Class::Title, "2;日本 title"),
            titled(Class::Title, "2;"),
            link("https://example.com/a?b=c#d"),
            link("http://example.com"),
            link("mailto:someone@example.com"),
            link("HTTPS://Example.com"),
            link(""),
        ]);
    }
    cases
}

fn refused_osc_cases() -> Vec<Case> {
    const UNLISTED: Refusal = Refusal::UnlistedOsc;
    const MALFORMED: Refusal = Refusal::MalformedOsc;
    const TARGET: Refusal = Refusal::HyperlinkTarget;
    const PARAMETERS: Refusal = Refusal::HyperlinkParameters;
    const READ: Refusal = Refusal::ClipboardRead;
    vec![
        dropped(UNLISTED, b"\x1b]1337;SetUserVar=foo=YmFy\x07"),
        dropped(UNLISTED, b"\x1b]1337;SetUserVar=foo=YmFy\x1b\\"),
        dropped(UNLISTED, b"\x1b]1337;File=inline=1:AAAA\x07"),
        dropped(UNLISTED, b"\x1b]9;notification\x07"),
        dropped(UNLISTED, b"\x1b]9;4;3\x07"),
        dropped(UNLISTED, b"\x1b]777;notify;title;body\x07"),
        dropped(UNLISTED, b"\x1b]99;i=1;body\x1b\\"),
        dropped(UNLISTED, b"\x1b]133;A\x07"),
        dropped(UNLISTED, b"\x1b]133;D;0\x07"),
        dropped(UNLISTED, b"\x1b]7;file:///tmp\x07"),
        dropped(UNLISTED, b"\x1b]4;1;rgb:ff/00/00\x07"),
        dropped(UNLISTED, b"\x1b]4;1;?\x07"),
        dropped(UNLISTED, b"\x1b]104\x07"),
        dropped(UNLISTED, b"\x1b]12;red\x07"),
        dropped(UNLISTED, b"\x1b]10;red\x07"),
        dropped(UNLISTED, b"\x1b]11;rgb:00/00/00\x07"),
        dropped(UNLISTED, b"\x1b]10;?;?\x07"),
        dropped(UNLISTED, b"\x1b]66;s=2;x\x07"),
        dropped(UNLISTED, b"\x1b];\x07"),
        dropped(UNLISTED, b"\x1b]123456789;x\x07"),
        dropped(UNLISTED, b"\x1b]x;y\x07"),
        dropped(READ, b"\x1b]52;c;?\x07"),
        dropped(READ, b"\x1b]52;;?\x1b\\"),
        dropped(MALFORMED, b"\x1b]52;c;not base64!\x07"),
        dropped(MALFORMED, b"\x1b]52;z;aGk=\x07"),
        dropped(MALFORMED, b"\x1b]52;c;\x07"),
        dropped(MALFORMED, b"\x1b]52\x07"),
        dropped(TARGET, b"\x1b]8;;javascript:alert(1)\x07"),
        dropped(TARGET, b"\x1b]8;;JavaScript:alert(1)\x07"),
        dropped(TARGET, b"\x1b]8;;file:///etc/passwd\x07"),
        dropped(TARGET, b"\x1b]8;;data:text/html,x\x07"),
        dropped(TARGET, b"\x1b]8;;ftp://example.com\x07"),
        dropped(TARGET, b"\x1b]8;;ssh://example.com\x07"),
        dropped(TARGET, b"\x1b]8;;vscode://x\x07"),
        dropped(TARGET, b"\x1b]8;;https://\x07"),
        dropped(TARGET, b"\x1b]8;;https:example.com\x07"),
        dropped(TARGET, b"\x1b]8;;https://exa mple.com\x07"),
        dropped(TARGET, "\x1b]8;;https://exämple.com\x07".as_bytes()),
        dropped(TARGET, b"\x1b]8;;example.com\x07"),
        dropped(TARGET, b"\x1b]8;;mailto:\x07"),
        dropped(PARAMETERS, b"\x1b]8;id=a;https://example.com\x07"),
        dropped(PARAMETERS, b"\x1b]8;foo=bar:id=a;https://example.com\x1b\\"),
        dropped(PARAMETERS, b"\x1b]8;id=a;\x07"),
        dropped(MALFORMED, b"\x1b]8;https://example.com\x07"),
        dropped(MALFORMED, b"\x1b]0;a\x01b\x07"),
        dropped(MALFORMED, b"\x1b]0;a\x7fb\x07"),
        dropped(MALFORMED, b"\x1b]0;\xff\x07"),
        dropped(MALFORMED, b"\x1b]0;\xc2\x9d0;x\x07"),
        dropped(MALFORMED, b"\x1b]2\x07"),
    ]
}

fn string_cases() -> Vec<Case> {
    const APC: Refusal = Refusal::Apc;
    const DCS: Refusal = Refusal::Dcs;
    const PM: Refusal = Refusal::Pm;
    const SOS: Refusal = Refusal::Sos;
    vec![
        dropped(APC, b"\x1b_Gf=100,a=T;AAAA\x1b\\"),
        dropped(APC, b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\"),
        dropped(DCS, b"\x1bPq#0;2;0;0;0#1;2;100;100;0~-\x1b\\"),
        dropped(DCS, b"\x1bP+q544e\x1b\\"),
        dropped(DCS, b"\x1bP$qm\x1b\\"),
        dropped(DCS, b"\x1bP1000p\x1b\\"),
        dropped(PM, b"\x1b^private message\x1b\\"),
        dropped(SOS, b"\x1bXstring\x1b\\"),
        dropped(APC, b"\x1b_unterminated by a bell\x07 still inside\x1b\\"),
    ]
}

fn csi_cases() -> Vec<Case> {
    vec![
        forwarded(Class::Sgr, b"\x1b[m"),
        forwarded(Class::Sgr, b"\x1b[0m"),
        forwarded(Class::Sgr, b"\x1b[1;31;4m"),
        forwarded(Class::Sgr, b"\x1b[38;5;196m"),
        forwarded(Class::Sgr, b"\x1b[38;2;1;2;3;48;2;4;5;6m"),
        forwarded(Class::Sgr, b"\x1b[4:3m"),
        forwarded(Class::Sgr, b"\x1b[38:2::1:2:3m"),
        forwarded(Class::CursorMove, b"\x1b[A"),
        forwarded(Class::CursorMove, b"\x1b[3B"),
        forwarded(Class::CursorMove, b"\x1b[3C"),
        forwarded(Class::CursorMove, b"\x1b[D"),
        forwarded(Class::CursorMove, b"\x1b[2E"),
        forwarded(Class::CursorMove, b"\x1b[F"),
        forwarded(Class::CursorMove, b"\x1b[5G"),
        forwarded(Class::CursorMove, b"\x1b[H"),
        forwarded(Class::CursorMove, b"\x1b[10;20H"),
        forwarded(Class::CursorMove, b"\x1b[;5H"),
        forwarded(Class::CursorMove, b"\x1b[1;1f"),
        forwarded(Class::CursorMove, b"\x1b[2d"),
        forwarded(Class::CursorMove, b"\x1b[`"),
        forwarded(Class::CursorMove, b"\x1b[a"),
        forwarded(Class::CursorMove, b"\x1b[e"),
        forwarded(Class::Erase, b"\x1b[2J"),
        forwarded(Class::Erase, b"\x1b[3J"),
        forwarded(Class::Erase, b"\x1b[K"),
        forwarded(Class::Erase, b"\x1b[1K"),
        forwarded(Class::Erase, b"\x1b[4X"),
        forwarded(Class::EditLines, b"\x1b[2@"),
        forwarded(Class::EditLines, b"\x1b[L"),
        forwarded(Class::EditLines, b"\x1b[3M"),
        forwarded(Class::EditLines, b"\x1b[P"),
        forwarded(Class::Scroll, b"\x1b[S"),
        forwarded(Class::Scroll, b"\x1b[2T"),
        forwarded(Class::ScrollRegion, b"\x1b[r"),
        forwarded(Class::ScrollRegion, b"\x1b[3;20r"),
        forwarded(Class::TabStops, b"\x1b[I"),
        forwarded(Class::TabStops, b"\x1b[Z"),
        forwarded(Class::TabStops, b"\x1b[g"),
        forwarded(Class::TabStops, b"\x1b[3g"),
        forwarded(Class::CursorSave, b"\x1b[s"),
        forwarded(Class::CursorSave, b"\x1b[u"),
        forwarded(Class::Repeat, b"\x1b[5b"),
        forwarded(Class::Modes, b"\x1b[?1000;1002;1006h"),
        forwarded(Class::Modes, b"\x1b[?2004l"),
        forwarded(Class::KittyKeyboard, b"\x1b[>u"),
        forwarded(Class::KittyKeyboard, b"\x1b[>1u"),
        forwarded(Class::KittyKeyboard, b"\x1b[<u"),
        forwarded(Class::KittyKeyboard, b"\x1b[<2u"),
        forwarded(Class::KittyKeyboard, b"\x1b[=5u"),
        forwarded(Class::KittyKeyboard, b"\x1b[=5;2u"),
        forwarded(Class::ModifyOtherKeys, b"\x1b[>4m"),
        forwarded(Class::ModifyOtherKeys, b"\x1b[>4;2m"),
        forwarded(Class::ModifyOtherKeys, b"\x1b[>4;m"),
        forwarded(Class::ModifyOtherKeys, b"\x1b[>4n"),
        trimmed(b"\x1b[?1049;2031h", b"\x1b[?1049h"),
        trimmed(b"\x1b[?1000;9999;1006l", b"\x1b[?1000;1006l"),
        trimmed(b"\x1b[?;2004h", b"\x1b[?2004h"),
        forwarded(Class::CursorStyle, b"\x1b[ q"),
        forwarded(Class::CursorStyle, b"\x1b[0 q"),
        forwarded(Class::CursorStyle, b"\x1b[6 q"),
        forwarded(Class::Reset, b"\x1b[!p"),
        owner_only(Class::TitleStack, b"\x1b[22;0t"),
        owner_only(Class::TitleStack, b"\x1b[23;2t"),
        asks(Query::PrimaryAttributes, b"\x1b[c"),
        asks(Query::PrimaryAttributes, b"\x1b[0c"),
        asks(Query::SecondaryAttributes, b"\x1b[>c"),
        asks(Query::SecondaryAttributes, b"\x1b[>0c"),
        asks(Query::DeviceStatus, b"\x1b[5n"),
        asks(Query::CursorPosition, b"\x1b[6n"),
        asks(Query::ExtendedCursorPosition, b"\x1b[?6n"),
        asks(Query::KeyboardFlags, b"\x1b[?u"),
        asks(Query::Version, b"\x1b[>q"),
        asks(Query::Version, b"\x1b[>0q"),
        asks(Query::ModeStatus(ModeQuery::Private(2026)), b"\x1b[?2026$p"),
        asks(Query::ModeStatus(ModeQuery::Ansi(4)), b"\x1b[4$p"),
        asks(Query::PixelSize, b"\x1b[14t"),
        asks(Query::CharSize, b"\x1b[18t"),
    ]
}

fn refused_csi_cases() -> Vec<Case> {
    const CSI: Refusal = Refusal::UnlistedCsi;
    const MODE: Refusal = Refusal::UnlistedMode;
    const MALFORMED: Refusal = Refusal::MalformedCsi;
    vec![
        dropped(CSI, b"\x1b[1c"),
        dropped(CSI, b"\x1b[=c"),
        dropped(CSI, b"\x1b[>1c"),
        dropped(CSI, b"\x1b[15n"),
        dropped(CSI, b"\x1b[?996n"),
        dropped(CSI, b"\x1b[?15n"),
        dropped(CSI, b"\x1b[8;24;80t"),
        dropped(CSI, b"\x1b[21t"),
        dropped(CSI, b"\x1b[2t"),
        dropped(CSI, b"\x1b[14;2t"),
        dropped(CSI, b"\x1b[1;2s"),
        dropped(CSI, b"\x1b[1;2;3;4;5T"),
        dropped(CSI, b"\x1b[1;2;3A"),
        dropped(CSI, b"\x1b[5i"),
        dropped(CSI, b"\x1b[1q"),
        dropped(CSI, b"\x1b[?1;1;0S"),
        dropped(CSI, b"\x1b[7 q"),
        dropped(CSI, b"\x1b[>1;2;3u"),
        dropped(CSI, b"\x1b[>5m"),
        dropped(CSI, b"\x1b[>4;3m"),
        dropped(CSI, b"\x1b[?5$q"),
        dropped(CSI, b"\x1b[?1;2$p"),
        dropped(CSI, b"\x1b[1!p"),
        dropped(MODE, b"\x1b[?3h"),
        dropped(MODE, b"\x1b[?1001h"),
        dropped(MODE, b"\x1b[?2048h"),
        dropped(MODE, b"\x1b[?1016h"),
        dropped(MODE, b"\x1b[?h"),
        dropped(MODE, b"\x1b[4h"),
        dropped(MODE, b"\x1b[20l"),
        dropped(MODE, b"\x1b[?99999999h"),
        dropped(MALFORMED, b"\x1b[1?m"),
        dropped(MALFORMED, b"\x1b[1$5m"),
        dropped(MALFORMED, b"\x1b[?1000:1h"),
        dropped(MALFORMED, b"\x1b[<>=?m"),
    ]
}

fn escape_cases() -> Vec<Case> {
    const ESCAPE: Refusal = Refusal::UnlistedEscape;
    vec![
        forwarded(Class::CursorSave, b"\x1b7"),
        forwarded(Class::CursorSave, b"\x1b8"),
        forwarded(Class::KeypadMode, b"\x1b="),
        forwarded(Class::KeypadMode, b"\x1b>"),
        forwarded(Class::Index, b"\x1bD"),
        forwarded(Class::Index, b"\x1bE"),
        forwarded(Class::Index, b"\x1bM"),
        forwarded(Class::TabStops, b"\x1bH"),
        forwarded(Class::Reset, b"\x1bc"),
        forwarded(Class::CharacterSet, b"\x1b(B"),
        forwarded(Class::CharacterSet, b"\x1b(0"),
        forwarded(Class::CharacterSet, b"\x1b)0"),
        forwarded(Class::CharacterSet, b"\x1b(A"),
        asks(Query::PrimaryAttributes, b"\x1bZ"),
        dropped(ESCAPE, b"\x1b G"),
        dropped(ESCAPE, b"\x1b F"),
        dropped(ESCAPE, b"\x1b#8"),
        dropped(ESCAPE, b"\x1b#3"),
        dropped(ESCAPE, b"\x1bN"),
        dropped(ESCAPE, b"\x1bO"),
        dropped(ESCAPE, b"\x1bn"),
        dropped(ESCAPE, b"\x1b\\"),
        dropped(ESCAPE, b"\x1b%G"),
        dropped(ESCAPE, b"\x1b(%5"),
        dropped(ESCAPE, b"\x1b*0"),
        dropped(ESCAPE, b"\x1b6"),
    ]
}

fn character_cases() -> Vec<Case> {
    const CONTROL: Refusal = Refusal::Control;
    const C1: Refusal = Refusal::StrayC1;
    const UTF8: Refusal = Refusal::InvalidUtf8;
    vec![
        plain(b"plain text, numbers 123 and ~punctuation~"),
        plain("日本語 é 😀 \u{1f468}\u{200d}\u{1f469}".as_bytes()),
        plain("\u{a0}\u{ff}\u{7ff}\u{800}\u{d7ff}\u{e000}\u{ffff}\u{10000}\u{10ffff}".as_bytes()),
        forwarded(Class::Control, b"\x07"),
        forwarded(Class::Control, b"\x08"),
        forwarded(Class::Control, b"\t"),
        forwarded(Class::Control, b"\n"),
        forwarded(Class::Control, b"\x0b"),
        forwarded(Class::Control, b"\x0c"),
        forwarded(Class::Control, b"\r"),
        forwarded(Class::Control, b"\x0e"),
        forwarded(Class::Control, b"\x0f"),
        dropped(CONTROL, b"\x00"),
        dropped(CONTROL, b"\x05"),
        dropped(CONTROL, b"\x18"),
        dropped(CONTROL, b"\x1a"),
        dropped(CONTROL, b"\x1c"),
        dropped(CONTROL, b"\x7f"),
        dropped(C1, b"\x9b"),
        dropped(C1, b"\x9d"),
        dropped(C1, b"\x90"),
        dropped(C1, b"\xc2\x9b"),
        dropped(C1, b"\xc2\x90"),
        dropped(C1, b"\xc2\x80"),
        dropped(C1, b"\xc2\x9f"),
        dropped(UTF8, b"\xff"),
        dropped(UTF8, b"\xa0"),
    ]
}

fn cases() -> Vec<Case> {
    let mut cases = Vec::new();
    cases.extend(character_cases());
    cases.extend(escape_cases());
    cases.extend(csi_cases());
    cases.extend(refused_csi_cases());
    cases.extend(mode_cases());
    cases.extend(osc_cases());
    cases.extend(refused_osc_cases());
    cases.extend(string_cases());
    cases.extend([
        asks(
            Query::Colour {
                palette: Palette::Foreground,
                terminator: Terminator::Bel,
            },
            b"\x1b]10;?\x07",
        ),
        asks(
            Query::Colour {
                palette: Palette::Background,
                terminator: Terminator::St,
            },
            b"\x1b]11;?\x1b\\",
        ),
    ]);
    cases
}

fn check(case: &Case, outcome: &Outcome, context: &str) {
    assert_eq!(
        String::from_utf8_lossy(&outcome.owner),
        String::from_utf8_lossy(case.owner),
        "owner stream {context} of {:?} ({})",
        String::from_utf8_lossy(case.input),
        case.name
    );
    assert_eq!(
        String::from_utf8_lossy(&outcome.watchers),
        String::from_utf8_lossy(case.watchers),
        "watcher stream {context} of {:?}",
        String::from_utf8_lossy(case.input)
    );
    assert_eq!(
        outcome.queries,
        case.queries,
        "queries {context} of {:?}",
        String::from_utf8_lossy(case.input)
    );
    assert_eq!(
        outcome.refused,
        tally(&case.refused),
        "refusals {context} of {:?}",
        String::from_utf8_lossy(case.input)
    );
}

#[test]
fn every_sequence_in_the_table_is_forwarded_answered_or_dropped_as_the_table_says() {
    for case in cases() {
        check(&case, &run(policy(Osc52::Off), &[case.input]), "alone");
    }
}

#[test]
fn a_sequence_surrounded_by_text_leaves_the_text_untouched() {
    for case in cases() {
        let mut input = b"before ".to_vec();
        input.extend_from_slice(case.input);
        input.extend_from_slice(b" after");
        let outcome = run(policy(Osc52::Off), &[&input]);
        let expect = |kept: &[u8]| {
            let mut out = b"before ".to_vec();
            out.extend_from_slice(kept);
            out.extend_from_slice(b" after");
            out
        };
        let context = format!("in text, {:?}", String::from_utf8_lossy(case.input));
        assert_eq!(outcome.owner, expect(case.owner), "owner {context}");
        assert_eq!(
            outcome.watchers,
            expect(case.watchers),
            "watchers {context}"
        );
        assert_eq!(outcome.queries, case.queries, "{context}");
    }
}

#[test]
fn a_sequence_split_at_every_byte_boundary_reads_the_same_as_unsplit() {
    for case in cases() {
        let whole = run(policy(Osc52::Off), &[case.input]);
        for cut in 1..case.input.len() {
            let (head, tail) = case.input.split_at(cut);
            let split = run(policy(Osc52::Off), &[head, tail]);
            assert_eq!(
                split,
                whole,
                "{:?} cut at {cut}",
                String::from_utf8_lossy(case.input)
            );
        }
        let singly: Vec<&[u8]> = case.input.chunks(1).collect();
        assert_eq!(
            run(policy(Osc52::Off), &singly),
            whole,
            "{:?} a byte at a time",
            String::from_utf8_lossy(case.input)
        );
    }
}

#[test]
fn the_whole_table_as_one_stream_reads_the_same_split_anywhere() {
    let stream: Vec<u8> = cases()
        .iter()
        .flat_map(|case| case.input.iter().copied().chain(*b" ."))
        .collect();
    let whole = run(policy(Osc52::Owner), &[&stream]);
    for cut in (1..stream.len()).step_by(7) {
        let (head, tail) = stream.split_at(cut);
        assert_eq!(
            run(policy(Osc52::Owner), &[head, tail]),
            whole,
            "cut at {cut}"
        );
    }
    let singly: Vec<&[u8]> = stream.chunks(1).collect();
    assert_eq!(run(policy(Osc52::Owner), &singly), whole);
}

#[test]
fn a_clipboard_write_goes_to_the_owner_only_and_only_when_the_host_was_told() {
    let write = b"\x1b]52;c;aGVsbG8=\x07";
    for end in [&b"\x07"[..], b"\x1b\\"] {
        let mut input = b"\x1b]52;pc;aGVsbG8=".to_vec();
        input.extend_from_slice(end);
        let allowed = run(policy(Osc52::Owner), &[&input]);
        assert_eq!(allowed.owner, input);
        assert_eq!(allowed.watchers, b"");
        assert_eq!(allowed.refused, tally(&[]));
    }
    let off = run(policy(Osc52::Off), &[write]);
    assert_eq!(off.owner, b"");
    assert_eq!(off.watchers, b"");
    assert_eq!(off.refused, tally(&[Refusal::ClipboardOff]));
    let read = run(policy(Osc52::Owner), &[b"\x1b]52;c;?\x07"]);
    assert_eq!(read.owner, b"");
    assert_eq!(read.refused, tally(&[Refusal::ClipboardRead]));
}

#[test]
fn a_query_says_how_much_of_the_forwarded_stream_came_before_it() {
    let mut egress = Egress::new(policy(Osc52::Off));
    let filtered = egress.feed(b"ab\x1b[6ncd\x1b]11;?\x07e");
    let at: Vec<usize> = filtered.asked.iter().map(|asked| asked.at).collect();
    assert_eq!(at, [2, 4]);
    assert_eq!(filtered.owner, b"abcde");
}

#[test]
fn a_query_split_across_reads_is_found_in_the_chunk_that_ends_it() {
    let mut egress = Egress::new(policy(Osc52::Off));
    assert_eq!(egress.feed(b"x\x1b[").asked.len(), 0);
    let ended = egress.feed(b"6ny");
    assert_eq!(ended.asked.len(), 1);
    assert_eq!(ended.asked.first().map(|asked| asked.at), Some(0));
}

struct Collect<'a>(&'a mut Outcome, Policy);

impl Sink for Collect<'_> {
    fn handle(&mut self, bytes: &[u8], class: Class) {
        match class.verdict(self.1) {
            Verdict::Forward(audience) => {
                if audience.includes(Recipient::Owner) {
                    self.0.owner.extend_from_slice(bytes);
                }
                if audience.includes(Recipient::Watcher) {
                    self.0.watchers.extend_from_slice(bytes);
                }
            }
            Verdict::Answer(query) => self.0.queries.push(query),
            Verdict::Drop(refusal) => self.0.refused.add(refusal),
        }
    }
}

fn read(input: &[u8]) -> (Parser, Outcome) {
    let mut parser = Parser::new();
    let mut outcome = Outcome::default();
    parser.feed(input, &mut Collect(&mut outcome, policy(Osc52::Owner)));
    (parser, outcome)
}

fn survives(input: &[u8]) -> Outcome {
    let (parser, outcome) = read(input);
    assert!(
        !parser.is_mid_sequence(),
        "the parser should have recovered"
    );
    outcome
}

fn long(start: &[u8], filler: u8, length: usize, end: &[u8]) -> Vec<u8> {
    let mut input = start.to_vec();
    input.extend(std::iter::repeat_n(filler, length));
    input.extend_from_slice(end);
    input
}

#[test]
fn a_string_being_skipped_runs_to_its_end_however_long_and_holds_nothing() {
    let cases: [(&[u8], &[u8], Refusal); 7] = [
        (b"\x1b]1337;File=inline=1:", b"\x07", Refusal::UnlistedOsc),
        (b"\x1b]1337;File=inline=1:", b"\x1b\\", Refusal::UnlistedOsc),
        (b"\x1bPq", b"\x1b\\", Refusal::Dcs),
        (b"\x1b_G", b"\x1b\\", Refusal::Apc),
        (b"\x1b^", b"\x1b\\", Refusal::Pm),
        (b"\x1bX", b"\x1b\\", Refusal::Sos),
        (b"\x1b]99999999;", b"\x07", Refusal::UnlistedOsc),
    ];
    for (start, end, refusal) in cases {
        let mut input = b"before ".to_vec();
        input.extend(long(start, b'A', 300 * 1024, end));
        input.extend_from_slice(b" after");
        let name = String::from_utf8_lossy(start).into_owned();
        let outcome = survives(&input);
        assert_eq!(outcome.owner, b"before  after", "{name}");
        assert_eq!(outcome.refused, tally(&[refusal]), "{name}");
        let (parser, _) = read(&input[..input.len() - 6]);
        assert_eq!(parser.held_len(), 0, "{name} kept what it skips");
    }
}

#[test]
fn a_string_that_never_ends_forwards_nothing_until_it_is_cancelled() {
    for start in [&b"\x1b]52;c;"[..], b"\x1b_G", b"\x1bP", b"\x1b]1337;"] {
        let input = long(start, b'A', 300 * 1024, b"never ends");
        let (parser, outcome) = read(&input);
        assert!(parser.is_mid_sequence(), "{start:?}");
        assert!(outcome.owner.is_empty(), "{start:?}");
        let mut cancelled = input;
        cancelled.extend_from_slice(b"\x18after");
        assert_eq!(survives(&cancelled).owner, b"after", "{start:?}");
    }
}

#[test]
fn a_kept_string_over_its_limit_is_dropped_whole_whatever_follows() {
    for (start, length) in [(&b"\x1b]0;"[..], 100 * 1024), (b"\x1b]52;c;", 300 * 1024)] {
        let input = long(start, b'T', length, b"\x07after");
        let outcome = survives(&input);
        assert_eq!(outcome.owner, b"after", "{start:?}");
        assert_eq!(outcome.refused, tally(&[Refusal::Overlong]), "{start:?}");
        let (parser, _) = read(&input[..input.len() - 6]);
        assert!(
            parser.held_len() < 5000,
            "{start:?} kept {}",
            parser.held_len()
        );
    }
}

#[test]
fn an_overlong_csi_is_read_to_its_final_byte_and_its_tail_is_never_printed() {
    for (opening, filler) in [(&b"\x1b["[..], b'1'), (b"\x1b[?", b'1'), (b"\x1b[1", b'$')] {
        let input = long(opening, filler, 1000, b"1;1Htext");
        let outcome = survives(&input);
        assert_eq!(outcome.owner, b"text", "{opening:?}");
        assert_eq!(outcome.refused, tally(&[Refusal::Overlong]), "{opening:?}");
    }
    let outcome = survives(&long(b"\x1b[", b'1', 1000, b"\x07\x1b[1mx"));
    assert_eq!(outcome.owner, b"\x07\x1b[1mx");
    let outcome = survives(&long(b"\x1b(", b' ', 1000, b"0text"));
    assert_eq!(outcome.owner, b"text");
    assert_eq!(outcome.refused, tally(&[Refusal::Overlong]));
}

#[test]
fn a_control_character_inside_a_csi_executes_without_splitting_it() {
    let outcome = survives(b"ab\x1b[2\x08;5Hx");
    assert_eq!(outcome.owner, b"ab\x08\x1b[2;5Hx");
    assert!(outcome.refused.is_empty());
    let outcome = survives(b"\x1b[1\x0a2\x7f\x01m");
    assert_eq!(outcome.owner, b"\x0a\x1b[12m");
    assert_eq!(
        outcome.refused,
        tally(&[Refusal::Control, Refusal::Control])
    );
}

#[test]
fn a_sequence_cancelled_or_interrupted_is_dropped_and_what_interrupts_it_is_read_afresh() {
    let aborted = |input: &[u8], owner: &[u8], refusals: &[Refusal]| {
        let outcome = run(policy(Osc52::Off), &[input]);
        assert_eq!(
            String::from_utf8_lossy(&outcome.owner),
            String::from_utf8_lossy(owner),
            "{input:?}"
        );
        assert_eq!(outcome.refused, tally(refusals), "{input:?}");
    };
    aborted(b"\x1b[3\x18m", b"m", &[Refusal::Aborted]);
    aborted(b"\x1b[3\x1am", b"m", &[Refusal::Aborted]);
    aborted(b"\x1b[3\x1b[31m", b"\x1b[31m", &[Refusal::Aborted]);
    aborted(b"\x1b[3\x18", b"", &[Refusal::Aborted]);
    aborted(b"\x1b[3\xc3\xa9", "é".as_bytes(), &[Refusal::Aborted]);
    aborted(b"\x1b]0;abc\x1b[31m", b"\x1b[31m", &[Refusal::Aborted]);
    aborted(b"\x1b]0;abc\x18x", b"x", &[Refusal::Aborted]);
    aborted(b"\x1b]1337;x\x1b[31m", b"\x1b[31m", &[Refusal::UnlistedOsc]);
    aborted(b"\x1bP1;2\x1b[31m", b"\x1b[31m", &[Refusal::Dcs]);
    aborted(b"\x1b\x1b[31m", b"\x1b[31m", &[Refusal::Aborted]);
    aborted(b"\x1b\x18x", b"x", &[Refusal::Aborted]);
    aborted(b"\x1b\nx", b"\nx", &[Refusal::Aborted]);
    aborted(b"\xe2\x82a", b"a", &[Refusal::InvalidUtf8]);
    aborted(b"\xe2\x82\x1b[0m", b"\x1b[0m", &[Refusal::InvalidUtf8]);
}

#[test]
fn nothing_but_whole_valid_items_is_forwarded_whatever_the_program_prints() {
    let mut noise = Vec::new();
    let mut state = 0x2545_f491_4f6c_dd1du64;
    for _ in 0..200_000 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        noise.push(state.to_le_bytes()[3]);
    }
    for policy in [policy(Osc52::Off), policy(Osc52::Owner)] {
        for chunk_size in [1usize, 3, 4096, 200_000] {
            let chunks: Vec<&[u8]> = noise.chunks(chunk_size).collect();
            let outcome = run(policy, &chunks);
            let again = run(policy, &[&outcome.owner]);
            assert_eq!(
                again.owner, outcome.owner,
                "what is forwarded passes the filter unchanged"
            );
            assert!(again.refused.is_empty(), "{}", again.refused);
            assert_eq!(again.queries.len(), 0);
            let watchers = run(policy, &[&outcome.watchers]);
            assert_eq!(watchers.watchers, outcome.watchers);
            assert!(watchers.refused.is_empty());
            let unsplit = run(policy, &[&noise]);
            assert_eq!(outcome, unsplit, "chunks of {chunk_size}");
        }
    }
}

#[test]
fn a_forwarded_stream_never_holds_a_c1_control_or_a_stray_escape_the_terminal_could_read() {
    let hostile: &[&[u8]] = &[
        b"\x9b31m",
        b"\xc2\x9b31m",
        b"\x9d0;x\x9c",
        b"\x1b]0;x\x9c",
        b"\x1bP\x9c",
        b"\x90q",
        b"\x1b[31",
        b"\x1b]0;x",
    ];
    for input in hostile {
        let outcome = run(policy(Osc52::Owner), &[input]);
        for stream in [&outcome.owner, &outcome.watchers] {
            let text = String::from_utf8_lossy(stream);
            assert!(
                !text.chars().any(|c| ('\u{80}'..='\u{9f}').contains(&c)),
                "{input:?} -> {stream:?}"
            );
            assert!(!stream.contains(&0x1b), "{input:?} -> {stream:?}");
            assert!(stream.iter().all(|byte| *byte < 0x80 || *byte >= 0xc2));
        }
    }
}

#[test]
fn a_replay_or_dump_goes_through_the_same_allowlist_for_its_recipient() {
    let replay = b"\x1b[0m\x1b[?1049h\x1b[1;1Hhi\x1b]2;a title\x1b\\\x1b[?1000h";
    let owner = Egress::new(policy(Osc52::Off)).clean(replay, Recipient::Owner);
    assert_eq!(owner.bytes, replay);
    assert!(owner.refused.is_empty());
    let watcher = Egress::new(policy(Osc52::Off)).clean(replay, Recipient::Watcher);
    assert_eq!(watcher.bytes, b"\x1b[0m\x1b[?1049h\x1b[1;1Hhi\x1b[?1000h");
    let dirty = Egress::new(policy(Osc52::Off)).clean(
        b"ok\x1b]1337;x\x07\x1b_G\x1b\\\x1b[?1001h",
        Recipient::Owner,
    );
    assert_eq!(dirty.bytes, b"ok");
    assert_eq!(dirty.refused.total(), 3);
}

fn every_forwarded_class_has_a_case(class: Class) -> bool {
    match class {
        Class::Text
        | Class::Control
        | Class::Sgr
        | Class::CursorMove
        | Class::Erase
        | Class::EditLines
        | Class::Scroll
        | Class::ScrollRegion
        | Class::TabStops
        | Class::CursorSave
        | Class::Repeat
        | Class::Index
        | Class::CharacterSet
        | Class::KeypadMode
        | Class::Reset
        | Class::Modes
        | Class::KittyKeyboard
        | Class::ModifyOtherKeys
        | Class::CursorStyle
        | Class::TitleStack
        | Class::Title
        | Class::Hyperlink => true,
        Class::Clipboard | Class::Answered(_) | Class::Refused(_) => false,
    }
}

const FORWARDED: [Class; 22] = [
    Class::Text,
    Class::Control,
    Class::Sgr,
    Class::CursorMove,
    Class::Erase,
    Class::EditLines,
    Class::Scroll,
    Class::ScrollRegion,
    Class::TabStops,
    Class::CursorSave,
    Class::Repeat,
    Class::Index,
    Class::CharacterSet,
    Class::KeypadMode,
    Class::Reset,
    Class::Modes,
    Class::KittyKeyboard,
    Class::ModifyOtherKeys,
    Class::CursorStyle,
    Class::TitleStack,
    Class::Title,
    Class::Hyperlink,
];

fn class_of(input: &[u8]) -> Option<Class> {
    match input {
        [0x1b, b'[', ..] => Some(match classify::csi(input) {
            Reading::Whole(class) => class,
            Reading::Trimmed(_) => Class::Modes,
        }),
        [0x1b, b']', ..] => {
            let terminator = if input.ends_with(b"\x07") {
                Terminator::Bel
            } else {
                Terminator::St
            };
            Some(classify::osc(input, terminator))
        }
        [0x1b, second, ..] if !matches!(second, b'P' | b'X' | b'^' | b'_') => {
            Some(classify::escape(input))
        }
        _ => None,
    }
}

#[test]
fn every_class_the_table_forwards_is_exercised_and_each_case_is_the_class_it_claims() {
    assert!(
        FORWARDED
            .iter()
            .all(|class| every_forwarded_class_has_a_case(*class))
    );
    let cases = cases();
    for class in FORWARDED {
        assert!(
            cases.iter().any(|case| case.class == Some(class)),
            "{class:?} has no case"
        );
    }
    for case in &cases {
        let (Some(claimed), Some(found)) = (case.class, class_of(case.input)) else {
            continue;
        };
        if claimed == Class::Refused(Refusal::MalformedCsi) {
            continue;
        }
        assert_eq!(found, claimed, "{:?}", String::from_utf8_lossy(case.input));
    }
}

#[test]
fn every_reason_a_sequence_is_dropped_is_exercised() {
    let cases = cases();
    let seen = |refusal: Refusal| {
        cases.iter().any(|case| case.refused.contains(&refusal))
            || matches!(
                refusal,
                Refusal::Overlong | Refusal::Aborted | Refusal::ClipboardOff
            )
    };
    for refusal in [
        Refusal::Control,
        Refusal::StrayC1,
        Refusal::InvalidUtf8,
        Refusal::UnlistedEscape,
        Refusal::UnlistedCsi,
        Refusal::UnlistedMode,
        Refusal::MalformedCsi,
        Refusal::Apc,
        Refusal::Dcs,
        Refusal::Pm,
        Refusal::Sos,
        Refusal::UnlistedOsc,
        Refusal::MalformedOsc,
        Refusal::HyperlinkTarget,
        Refusal::HyperlinkParameters,
        Refusal::ClipboardRead,
    ] {
        assert!(seen(refusal), "{refusal:?}");
    }
}

#[test]
fn the_audience_of_each_class_is_as_the_table_says() {
    let owner_only = [Class::Title, Class::TitleStack];
    for class in FORWARDED {
        let expected = if owner_only.contains(&class) {
            Audience::Owner
        } else {
            Audience::Everyone
        };
        assert_eq!(
            class.verdict(policy(Osc52::Off)),
            Verdict::Forward(expected),
            "{class:?}"
        );
    }
    assert_eq!(
        Class::Clipboard.verdict(policy(Osc52::Owner)),
        Verdict::Forward(Audience::Owner)
    );
    assert_eq!(
        Class::Clipboard.verdict(policy(Osc52::Off)),
        Verdict::Drop(Refusal::ClipboardOff)
    );
}

#[test]
fn only_the_listed_modes_are_forwarded() {
    for mode in 0..=u16::MAX {
        let set = format!("\x1b[?{mode}h");
        let outcome = run(policy(Osc52::Off), &[set.as_bytes()]);
        let listed = Mode::from_number(mode).is_some();
        assert_eq!(!outcome.owner.is_empty(), listed, "mode {mode}");
    }
}

#[test]
fn malformed_utf8_is_never_forwarded_and_the_text_after_it_is() {
    for broken in [
        &b"\xc0\xaf"[..],
        b"\xf5\x80\x80\x80",
        b"\xed\xa0\x80",
        b"\xe0\x80\x80",
        b"\xf4\x90\x80\x80",
        b"\xe2\x82",
        b"\xf0\x9f\x98",
    ] {
        let mut input = broken.to_vec();
        input.extend_from_slice(b" after");
        let outcome = run(policy(Osc52::Off), &[&input]);
        assert_eq!(outcome.owner, b" after", "{broken:?}");
        assert!(outcome.refused.total() >= 1, "{broken:?}");
    }
}
