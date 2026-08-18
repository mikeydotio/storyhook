//! Pins every Enter-submitting text input in the dashboard to one guarded
//! binding (SH-362).
//!
//! Four inputs bound `keydown` and called a submit function when
//! `e.key === "Enter"`, with no `event.repeat` check. Measured against the
//! unfixed build, one held Enter issued **nine** DELETEs from `#delete-reason`
//! and **nine** `POST /token` from `#token-input`; the delete path's extra
//! requests failed into a field inside a modal the first success had already
//! closed, so the user saw one success and nothing else. `bindEnterSubmit` is
//! the repair, and `e2e/specs/modal-enter-autorepeat.spec.ts` proves it works.
//!
//! This file exists for a different question: whether the **next** such input
//! can ship unguarded. It can, and did — twice. The story was filed naming
//! three sites; a sibling sweep found a fourth (`buildStatusAddForm`); and the
//! sweep itself missed a fifth, `buildLabelsSection`, because it is spelled
//! `if (e.key !== "Enter" || …) return;` and every grep in the investigation
//! had been written `e.key === "Enter"`. A sweep that misses a site is exactly
//! the class this repository has already paid for four times (SH-136, SH-198,
//! SH-260, SH-364), and the standing answer is a derived check rather than a
//! hand-maintained list.
//!
//! ## The predicate, and why it is this one
//!
//! After the fix, a correctly wired site contains **no `"Enter"` literal at
//! all** — `bindEnterSubmit($("delete-reason"), submitDeleteStory)` names
//! neither the key nor the event. So the rule is simply:
//!
//! > Every line of dashboard *code* containing the literal `"Enter"` must be a
//! > known non-submitting idiom.
//!
//! Matching the **raw literal** rather than `e.key === "Enter"` is the whole
//! point, and was a condition of the council verdict: the one site everybody
//! missed is spelled with an inverted test, and a predicate keyed to `===`
//! would have walked past it in silence a third time.
//!
//! ## What the allowlist is, and what it is not
//!
//! The allowlisted idioms are matched **semantically** — by what else the line
//! says — not by line number or by exact text, so ordinary edits above them do
//! not break this test and a reformatted line does not need re-blessing. Each
//! entry states the property that makes it not-a-submit:
//!
//! * an Enter-**or-Space** activation (`|| e.key === " "`) is a button/menu/card
//!   keyboard activation, not a text-input submit — SH-339's guard is the one
//!   that covers those, on the button side;
//! * `.blur()` is the blur-on-Enter idiom, which commits through the field's own
//!   `blur` handler and cannot repeat, since after the first Enter the target is
//!   no longer the input;
//! * the label **combobox** (`e.key === ","`) adds a chip client-side and
//!   reaches no network;
//! * the two guards themselves, which necessarily name the key they guard.
//!
//! This is a list of *exceptions*, not a list of sites to check, and the
//! difference is the failure mode: a new Enter-submitting input written inline
//! is not on it and fails **by name**, whereas a missed site on a hand-kept
//! roster fails by nothing at all.
//!
//! ## What this test cannot catch — knowingly traded away
//!
//! It is a **wiring** fence, not a behaviour fence. It proves each site funnels
//! through one helper; that the helper's own logic is right is
//! `modal-enter-autorepeat.spec.ts`'s job, including the both-directions
//! mutation battery. And it is keyed to one spelling, so it would miss
//! `e.keyCode === 13`, `e.code === "Enter"`, or a handler installed by
//! `input.onkeydown = …`. None of those appear in this file today (asserted
//! below, so "none appear" stays a measurement rather than a memory), but a
//! determined dodge is possible and the next person to add a site should know
//! that rather than rediscover it. The alternative the council weighed — one
//! delegated `document`-capture listener, immune to spelling because it never
//! reads the site's own code — was rejected for being **not fail-safe**: see
//! `bindEnterSubmit`'s own doc comment, and the verdict recorded on **SH-362**
//! (`story show SH-362`).
//!
//! The council's full trail — proposals, the vote, two seats retracting their
//! own proposals — was written to an untracked, worktree-local directory and
//! is gone, which is the defect SH-363 was filed for and has now fixed: the
//! verdict on the story is the record, and no tracked file names such a
//! directory any more.
//!
//! The second thing it cannot catch, and every seat agreed on this: a new site
//! can call `bindEnterSubmit` correctly and still forget its **in-flight
//! claim**. Layer two is not statically decidable in JavaScript, and this file
//! does not pretend otherwise.
//!
//! ## The second property: one door for every key that reaches a field (SH-368)
//!
//! The scan above is keyed to `"Enter"`, which is the right question for a
//! *submit* and the wrong one for everything else a field's keydown handler
//! does. SH-368 found four more handlers reading other keys on a text field —
//! the drawer title's Enter-blurs, the status row description's, the label
//! combobox's Enter/comma/Backspace/Escape, and the description textarea's
//! Escape-reverts — plus the two page-level Escape handlers on `document`,
//! which see every field's keys on the way up. None of them submits anything,
//! so none of them was ever in the first scan's sight.
//!
//! [`every_keydown_listener_that_can_see_a_field_goes_through_bind_typed_keys`]
//! is the fence for that wider class, and it is keyed to the **receiver** of
//! each raw `addEventListener("keydown", …)` rather than to any key name: an
//! element a user can type into, or `document`, must be bound through
//! `bindTypedKeys`, while a menu, a button, a card or the notice dock may bind
//! directly because nothing composed can reach it. A receiver is admitted by
//! naming it and saying why it hosts no typing — the same "list of exceptions,
//! not a list of sites" shape as `ALLOWED` above, and the same failure mode: a
//! new `input.addEventListener("keydown", …)` is on nobody's list and fails by
//! name.
//!
//! Its own knowingly-traded limit is the mirror of the Enter scan's: it reads
//! the receiver's **spelling**, so a text field held in a variable named
//! something else would pass. That is why the receiver list is a list of
//! *specific names already in the file* rather than a rule like "not `input`" —
//! an unfamiliar receiver fails and gets read by a human, which is the only
//! mechanism available for a question no scan can answer.

use std::path::PathBuf;

/// The repository root, which is this package's manifest directory.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Reads a repository file, failing with the path rather than with `None` — a
/// missing dashboard file is a finding, not a reason to skip.
fn read(relative: &str) -> String {
    let path: PathBuf = repo_root().join(relative);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} must be readable: {error}", path.display()))
}

/// `source` with `//` line comments and `/* … */` blocks blanked out, preserving
/// line structure so a finding can still name its line number.
///
/// Blanked rather than removed, and character-for-character: the scan reports
/// 1-based line numbers straight back to the reader, and a stripper that
/// deleted lines would report numbers that do not exist in the file.
///
/// Crude by design — it does not know about strings, so a `//` inside a string
/// literal would blank the rest of that line. That direction of error can only
/// ever *hide* a site, which is why
/// [`the_scanner_still_finds_the_code_occurrences_it_is_supposed_to`] measures
/// what survives stripping instead of trusting it.
fn strip_comments(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let bytes: Vec<char> = source.chars().collect();
    let mut i = 0;
    let mut in_block = false;
    let mut in_line = false;
    while i < bytes.len() {
        let c = bytes[i];
        let next = bytes.get(i + 1).copied().unwrap_or('\0');
        if c == '\n' {
            in_line = false;
            out.push('\n');
            i += 1;
            continue;
        }
        if in_block {
            if c == '*' && next == '/' {
                in_block = false;
                out.push(' ');
                out.push(' ');
                i += 2;
                continue;
            }
            out.push(' ');
            i += 1;
            continue;
        }
        if in_line {
            out.push(' ');
            i += 1;
            continue;
        }
        if c == '/' && next == '*' {
            in_block = true;
            out.push(' ');
            out.push(' ');
            i += 2;
            continue;
        }
        if c == '/' && next == '/' {
            in_line = true;
            out.push(' ');
            out.push(' ');
            i += 2;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Why a line naming `"Enter"` is allowed to do so: the substring that must
/// also appear on it, and the reason that substring settles the question.
struct Idiom {
    /// A substring whose presence on the same line proves this is not a
    /// text-input submit.
    marker: &'static str,
    /// What that marker means, quoted in the failure message so a reader who
    /// trips this test learns the rule rather than the line number.
    reason: &'static str,
}

/// Every shape of `"Enter"` occurrence that is not an Enter-submitting text
/// input, keyed by what else the line says rather than by where it sits.
const ALLOWED: &[Idiom] = &[
    Idiom {
        marker: "e.key === \" \"",
        reason: "an Enter-or-Space keyboard activation of a button, menu item or card — \
                 not a text-input submit; the button side is SH-339's own guard",
    },
    Idiom {
        marker: ".blur()",
        reason: "the blur-on-Enter idiom, which commits through the field's `blur` handler; \
                 after the first Enter the target is no longer the input, so it cannot repeat",
    },
    Idiom {
        marker: "e.key === \",\"",
        reason: "the label combobox, which adds a chip client-side and reaches no network",
    },
    Idiom {
        marker: "!event.repeat",
        reason: "`refuseAutoRepeatActivation`, SH-339's guard for held Enter on a button",
    },
    Idiom {
        marker: "if (e.key !== \"Enter\") return;",
        reason: "`bindEnterSubmit`'s own check — the one place this file is allowed to \
                 read the key for a text-input submit",
    },
];

/// Every 1-based line of `src/web_dashboard.html` code (comments stripped) that
/// names `"Enter"`, paired with the line itself.
fn enter_lines() -> Vec<(usize, String)> {
    strip_comments(&read("src/web_dashboard.html"))
        .lines()
        .enumerate()
        .filter(|(_, line)| line.contains("\"Enter\""))
        .map(|(index, line)| (index + 1, line.trim().to_string()))
        .collect()
}

/// Whether `line` matches a known non-submitting idiom, and which.
fn allowed_idiom(line: &str) -> Option<&'static Idiom> {
    ALLOWED.iter().find(|idiom| line.contains(idiom.marker))
}

#[test]
fn every_enter_submitting_input_goes_through_bind_enter_submit() {
    let offenders: Vec<(usize, String)> = enter_lines()
        .into_iter()
        .filter(|(_, line)| allowed_idiom(line).is_none())
        .collect();

    assert!(
        offenders.is_empty(),
        "src/web_dashboard.html names \"Enter\" outside every known non-submitting idiom, at:\n{}\n\n\
         An input that submits on Enter must be bound with `bindEnterSubmit(input, submit)`, which \
         ignores auto-repeats — a hand-written `keydown` listener does not, and one held key then \
         issues one request per OS auto-repeat (SH-362: nine DELETEs, measured). A correctly wired \
         site names no key at all, which is why any \"Enter\" left in the code is either a guard, a \
         known non-submitting idiom, or this defect. The idioms this test recognises are:\n{}\n\n\
         If the line above genuinely is none of those, it needs an entry in ALLOWED with the \
         property that makes it not-a-submit — not an exemption.",
        offenders
            .iter()
            .map(|(number, line)| format!("  src/web_dashboard.html:{number}: {line}"))
            .collect::<Vec<_>>()
            .join("\n"),
        ALLOWED
            .iter()
            .map(|idiom| format!("  {:?} — {}", idiom.marker, idiom.reason))
            .collect::<Vec<_>>()
            .join("\n"),
    );
}

#[test]
fn the_scanner_still_finds_the_code_occurrences_it_is_supposed_to() {
    // The positive control, and the reason it is not optional: every assertion
    // above is satisfied *perfectly* by a scanner that found nothing at all —
    // a comment stripper that swallowed real code, a predicate that stopped
    // matching, a file moved out from under `read()`. This is SH-364's lesson
    // (a check that agrees with itself passes while matching zero rows), and
    // the two known guards are the fixture-free way to state it, since both
    // must name the key by construction.
    let lines = enter_lines();
    assert!(
        lines.len() >= 8,
        "the scan found only {} code lines naming \"Enter\" in src/web_dashboard.html, which is \
         fewer than the keyboard-activation idioms alone — the comment stripper or the predicate \
         has stopped seeing code, and this file is reporting a clean tree it never read",
        lines.len()
    );
    for marker in ["!event.repeat", "if (e.key !== \"Enter\") return;"] {
        assert!(
            lines.iter().any(|(_, line)| line.contains(marker)),
            "the scan did not find {marker:?} — both auto-repeat guards must survive comment \
             stripping, or this scan is measuring something other than the file's code"
        );
    }
}

#[test]
fn the_comment_stripper_hides_a_doc_comment_without_hiding_the_code_beside_it() {
    // Both directions of the stripper, on one fixture. `src/web_dashboard.html`
    // genuinely does mention `event.key === "Enter"` inside doc comments; if
    // those reached the scan they would have to be allowlisted, and the
    // allowlist would then mean two different things — "not a submit" and "not
    // code" — which is how an exception list stops being readable.
    let stripped = strip_comments(
        "/** Doc: `event.key === \"Enter\"` covers NumpadEnter. */\n\
         var kept = \"Enter\"; // trailing: \"Enter\"\n\
         /* block\n\
            \"Enter\"\n\
         */\n\
         var also = \"Enter\";\n",
    );
    let hits: Vec<usize> = stripped
        .lines()
        .enumerate()
        .filter(|(_, line)| line.contains("\"Enter\""))
        .map(|(index, _)| index + 1)
        .collect();
    assert_eq!(
        hits,
        vec![2, 6],
        "the stripper must blank doc comments, trailing `//` comments and `/* */` blocks while \
         leaving the code beside them — and must preserve line numbering, since the scan reports \
         line numbers straight to the reader. Surviving lines were {hits:?}"
    );
}

#[test]
fn an_inline_enter_submit_is_rejected_however_it_is_spelled() {
    // The mutation check, run against the two spellings this defect actually
    // wore rather than an invented one. The `!==` form is `buildLabelsSection`
    // (SH-362's fifth site) verbatim in shape: it is the site the filing missed
    // AND the sibling sweep missed, because every grep in the investigation had
    // been written `===`. A predicate keyed to `===` passes this test's first
    // case and fails the second, which is precisely the hole the council
    // required closing — so both are asserted, not just the one that bit.
    for spelling in [
        "input.addEventListener(\"keydown\", function(e) { if (e.key === \"Enter\") submitThing(); });",
        "if (e.key !== \"Enter\" || !input.value.trim()) return;",
        "if (e.key == \"Enter\") { submitThing(); }",
    ] {
        assert!(
            allowed_idiom(spelling).is_none(),
            "an inline Enter-submit spelled {spelling:?} was accepted by an ALLOWED idiom — the \
             allowlist has grown a marker broad enough to cover the defect this test exists to \
             catch"
        );
        assert!(
            strip_comments(spelling).contains("\"Enter\""),
            "the stripper removed {spelling:?}, which is code — a scan that cannot see this line \
             cannot report it"
        );
    }
}

#[test]
fn the_dashboard_reads_the_enter_key_by_name_and_not_by_code() {
    // The spelling-immunity this fence knowingly gives up, kept honest by
    // measurement instead of memory. `bindEnterSubmit`'s doc comment records
    // that `e.keyCode === 13`, `e.code === "Enter"` and `onkeydown =` would all
    // slip past the scan above; that trade is only acceptable while none of
    // them is in use, and "none is in use" is a fact about today's file, not a
    // property of it. If one arrives, this fails and the trade gets re-argued.
    let code = strip_comments(&read("src/web_dashboard.html"));
    for dodge in ["keyCode", ".code ===", "onkeydown ="] {
        assert!(
            !code.contains(dodge),
            "src/web_dashboard.html now uses {dodge:?}, a spelling of key handling that \
             `every_enter_submitting_input_goes_through_bind_enter_submit` cannot see. Either \
             write the handler through `bindEnterSubmit` and drop this spelling, or widen that \
             scan and update `bindEnterSubmit`'s doc comment, which currently promises a reader \
             that these spellings are absent"
        );
    }
}

/// Why a keydown listener may be wired straight onto its receiver instead of
/// going through `bindTypedKeys`: the receiver's spelling, and what makes it
/// impossible for an in-progress IME composition to reach it.
struct Receiver {
    /// The expression the listener is registered on, verbatim, as it appears
    /// left of `.addEventListener("keydown"`.
    name: &'static str,
    /// Why nothing typed can reach it — quoted in the failure message, so a
    /// reader who trips this test learns the distinction rather than the line.
    reason: &'static str,
}

/// Every receiver allowed to bind `keydown` directly.
///
/// `document` is deliberately absent: a page-level listener sees the keys of
/// whichever field has focus, on the way up, so it is on the typing side of
/// this line and belongs behind the door with the fields.
const DIRECT_KEYDOWN_RECEIVERS: &[Receiver] = &[
    Receiver {
        name: "target",
        reason: "`bindTypedKeys` itself — the one door, and the only listener in \
                 this file registered on a receiver it does not know",
    },
    Receiver {
        name: "storyMenuNode",
        reason: "the story context menu: a `role=\"menu\"` of buttons, none of \
                 which accepts text",
    },
    Receiver {
        name: "storySubmenuNode",
        reason: "the story context menu's submenu — the same, one level down",
    },
    Receiver {
        name: "columnSortMenuNode",
        reason: "the column sort menu — the same shape again",
    },
    Receiver {
        name: "container",
        reason: "the board column's roving-focus card container: the focused \
                 element is a card, which hosts no composition",
    },
    Receiver {
        name: "view",
        reason: "the drawer description's read-only view, which Enter/Space swap \
                 for the textarea — the textarea itself is bound through the door",
    },
    Receiver {
        name: "btn",
        reason: "the project selector's own button",
    },
    Receiver {
        name: "menu",
        reason: "the project selector's menu of buttons",
    },
    Receiver {
        name: "$(\"notice-dock\")",
        reason: "the notice dock, `refuseAutoRepeatActivation`'s only mount — a \
                 stack of dismiss buttons",
    },
];

/// Every 1-based line of dashboard code registering a `keydown` listener
/// directly, paired with the receiver it registers on.
fn direct_keydown_bindings() -> Vec<(usize, String)> {
    strip_comments(&read("src/web_dashboard.html"))
        .lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let (receiver, _) = line.split_once(".addEventListener(\"keydown\"")?;
            Some((index + 1, receiver.trim().to_string()))
        })
        .collect()
}

#[test]
fn every_keydown_listener_that_can_see_a_field_goes_through_bind_typed_keys() {
    let offenders: Vec<(usize, String)> = direct_keydown_bindings()
        .into_iter()
        .filter(|(_, receiver)| {
            !DIRECT_KEYDOWN_RECEIVERS
                .iter()
                .any(|allowed| allowed.name == receiver)
        })
        .collect();

    assert!(
        offenders.is_empty(),
        "src/web_dashboard.html binds `keydown` directly on a receiver this test does not \
         recognise, at:\n{}\n\n\
         A listener that can see a text field's keys — one bound on the field itself, or on \
         `document`, which sees every field's keys as they bubble — must be registered through \
         `bindTypedKeys(target, handler)`, which refuses events the user's input method is still \
         composing (SH-368: an Enter that commits a composition submitted six dashboard forms \
         mid-word). Binding directly is only for a receiver nothing composed can reach, and the \
         ones this file already has are:\n{}\n\n\
         If the receiver above genuinely hosts no typing, it needs an entry naming that property \
         — not an exemption.",
        offenders
            .iter()
            .map(|(number, receiver)| format!(
                "  src/web_dashboard.html:{number}: {receiver}.addEventListener(\"keydown\", …)"
            ))
            .collect::<Vec<_>>()
            .join("\n"),
        DIRECT_KEYDOWN_RECEIVERS
            .iter()
            .map(|allowed| format!("  {:?} — {}", allowed.name, allowed.reason))
            .collect::<Vec<_>>()
            .join("\n"),
    );
}

#[test]
fn the_receiver_scan_still_finds_the_bindings_it_is_supposed_to() {
    // The positive control, for the reason the Enter scan already states: a
    // scan that found nothing satisfies the assertion above perfectly. The
    // door's own binding is the fixture-free anchor — it is the one line that
    // must exist for `bindTypedKeys` to be a door at all.
    let bindings = direct_keydown_bindings();
    assert!(
        bindings.len() >= 8,
        "the scan found only {} direct `keydown` bindings in src/web_dashboard.html, fewer than \
         the menu and roving-focus receivers alone — the comment stripper or the predicate has \
         stopped seeing code, and this file is reporting a clean tree it never read",
        bindings.len()
    );
    assert!(
        bindings.iter().any(|(_, receiver)| receiver == "target"),
        "the scan did not find `bindTypedKeys`'s own `target.addEventListener(\"keydown\", …)` — \
         either the door has been renamed and this file was not updated with it, or the scan is \
         measuring something other than this file's code"
    );
}

#[test]
fn a_hand_wired_field_listener_is_rejected_however_its_receiver_is_named() {
    // The mutation check, run against the four receivers SH-368 actually
    // converted plus the `document` one, rather than an invented name. Every
    // one of these was in the file before that story and would have to be
    // rejected if it came back.
    for receiver in ["input", "textarea", "document", "description", "field"] {
        let line = format!("{receiver}.addEventListener(\"keydown\", function(e) {{ act(e); }});");
        assert!(
            !DIRECT_KEYDOWN_RECEIVERS
                .iter()
                .any(|allowed| allowed.name == receiver),
            "{receiver:?} was accepted as a direct `keydown` receiver — a text field, or \
             `document`, has been admitted to a list whose whole subject is receivers that host \
             no typing"
        );
        assert!(
            strip_comments(&line).contains(".addEventListener(\"keydown\""),
            "the stripper removed {line:?}, which is code — a scan that cannot see this line \
             cannot report it"
        );
    }
}

#[test]
fn a_keydown_listener_cannot_be_smuggled_in_as_an_element_property() {
    // The receiver scan reads `x.addEventListener("keydown", …)` and nothing
    // else, so `el()`'s own `on*` props are a second door it cannot see:
    // `el("input", { onKeydown: … })` registers a listener through
    // `node.addEventListener(key.slice(2).toLowerCase(), value)`, spelling
    // neither the receiver nor the event where any scan above would find them.
    // SH-368 removed the one site that used it (the status row's description
    // field); this keeps it removed. Lowercased before matching because `el()`
    // lowercases too, so `onKeyDown` and `onKEYDOWN` are the same door.
    let code = strip_comments(&read("src/web_dashboard.html")).to_lowercase();
    assert!(
        !code.contains("onkeydown"),
        "src/web_dashboard.html registers a `keydown` listener through an `el()` property or an \
         `onkeydown` assignment, which `every_keydown_listener_that_can_see_a_field_goes_through_\
         bind_typed_keys` cannot see. Write it as `bindTypedKeys(node, handler)` — or, if the \
         receiver hosts no typing, as an ordinary `addEventListener` call the scan can read"
    );
}
