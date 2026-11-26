use super::*;

fn diagram_style() -> MarkdownStyle {
    MarkdownStyle {
        mermaid: true,
        ..MarkdownStyle::default()
    }
}

fn visible(rows: &[Line]) -> Vec<String> {
    rows.iter()
        .map(|row| row.iter().map(|span| span.content.as_str()).collect())
        .collect()
}

const CLASS: &str =
    "```mermaid\nclassDiagram\n  Animal <|-- Dog\n  class Animal {\n    +speak()\n  }\n```";
const STATE: &str = "```mermaid\nstateDiagram-v2\n  [*] --> Idle\n  Idle --> Busy: go\n```";

#[test]
fn class_fence_draws_its_diagram() {
    assert_eq!(
        visible(&render_markdown(CLASS, 60, &diagram_style())),
        [
            "  ┌──────────┐",
            "  │  Animal  │",
            "  ├──────────┤",
            "  │ +speak() │",
            "  └─────△────┘",
            "        │",
            "        │",
            "     ┌─────┐",
            "     │ Dog │",
            "     └─────┘",
        ]
    );
}

#[test]
fn state_fence_draws_its_diagram() {
    assert_eq!(
        visible(&render_markdown(STATE, 60, &diagram_style())),
        [
            "    ╭───╮",
            "    │ ● │",
            "    ╰─┬─╯",
            "      │",
            "      ▼",
            "  ╭──────╮",
            "  │ Idle │",
            "  ╰───┬──╯",
            "      │",
            "      ▼go",
            "  ╭──────╮",
            "  │ Busy │",
            "  ╰──────╯",
        ]
    );
}

#[test]
fn mermaid_off_keeps_the_fence_as_code() {
    assert_eq!(
        visible(&render_markdown(STATE, 60, &MarkdownStyle::default())),
        [
            "  stateDiagram-v2",
            "    [*] --> Idle",
            "    Idle --> Busy: go",
        ]
    );
}

#[test]
fn mermaid_rows_count_what_they_paint_at_every_width() {
    let text = format!("intro\n\n{CLASS}\n\nbetween\n\n{STATE}\n\nafter");
    let style = diagram_style();
    for width in [1, 4, 12, 24, 40, 80, 200] {
        assert_eq!(
            markdown_row_count(&text, width, &style),
            render_markdown(&text, width, &style).len(),
            "width {width}"
        );
    }
}
