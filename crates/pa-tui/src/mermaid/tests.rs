use super::*;

/// The diagram's visible text, one trimmed string per row.
fn plain_lines(art: &MermaidArt) -> Vec<String> {
    art.styled_lines
        .iter()
        .map(|line| {
            let text: String = line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect();
            text.trim_end().to_string()
        })
        .collect()
}

fn styles() -> MermaidStyles {
    let s = Style::default();
    MermaidStyles {
        border: s,
        node_text: s,
        edge: s,
        edge_label: s,
        title: s,
    }
}

fn node(g: &Graph, i: usize) -> &Node {
    let Some(n) = g.nodes.get(i) else {
        panic!("expected node {i}, have {}", g.nodes.len());
    };
    n
}

fn edge(g: &Graph, i: usize) -> &Edge {
    let Some(e) = g.edges.get(i) else {
        panic!("expected edge {i}, have {}", g.edges.len());
    };
    e
}

fn index_of(g: &Graph, id: &str) -> usize {
    let Some(&i) = g.index.get(id) else {
        panic!("missing node id {id:?}");
    };
    i
}

fn at<T: Copy + std::fmt::Debug>(xs: &[T], i: usize) -> T {
    let Some(&v) = xs.get(i) else {
        panic!("index {i} out of bounds for {xs:?}");
    };
    v
}

fn plain(src: &str) -> String {
    plain_lines(&render(src, &styles(), Some(120)).unwrap()).join("\n")
}

/// The graph as (from id, to id, label) triples, ids looked up through `index`, sorted.
fn edge_ids(graph: &Graph) -> Vec<(String, String, Option<String>)> {
    let ids: HashMap<usize, &str> = graph
        .index
        .iter()
        .map(|(id, &i)| (i, id.as_str()))
        .collect();
    let mut out: Vec<(String, String, Option<String>)> = graph
        .edges
        .iter()
        .map(|e| {
            (
                ids[&e.from].to_string(),
                ids[&e.to].to_string(),
                e.label.clone(),
            )
        })
        .collect();
    out.sort();
    out
}

#[test]
fn parses_nodes_edges_and_direction() {
    let g = parse_graph("flowchart LR\n  A[Start] --> B[End]").unwrap();
    assert_eq!(g.nodes.len(), 2);
    assert_eq!(g.edges.len(), 1);
    assert_eq!(node(&g, 0).label, "Start");
    assert_eq!(node(&g, 1).label, "End");
    assert!(g.dir == Dir::Right);
}

#[test]
fn non_flowchart_returns_none_from_parse() {
    assert!(parse_graph("sequenceDiagram\n  A->>B: hi").is_none());
}

#[test]
fn html_tags_are_stripped_from_labels() {
    let g = parse_graph("flowchart TD\n  A[\"<b>Bold</b> and <i>italic</i>\"] --> B").unwrap();
    assert_eq!(node(&g, 0).label, "Bold and italic");
}

#[test]
fn br_tag_becomes_a_space() {
    let g = parse_graph("flowchart TD\n  A[\"Line1<br/>Line2<br>Line3\"]").unwrap();
    assert_eq!(node(&g, 0).label, "Line1 Line2 Line3");
}

#[test]
fn markdown_string_strips_bold_italic_and_code() {
    let g = parse_graph(
        "flowchart TD\n  A[\"`**Start** here`\"] --> B[\"`Save to **database**`\"]\n  B --> C[\"`**Done!**`\"]",
    )
    .unwrap();
    assert_eq!(node(&g, 0).label, "Start here");
    assert_eq!(node(&g, 1).label, "Save to database");
    assert_eq!(node(&g, 2).label, "Done!");
}

#[test]
fn markdown_string_preserves_snake_case_and_strips_inline_code() {
    let g =
        parse_graph("flowchart TD\n  A[\"`_italic_ uses `vocab_size` with __all__`\"]").unwrap();
    assert_eq!(node(&g, 0).label, "italic uses vocab_size with all");
}

#[test]
fn markdown_string_edge_label_is_stripped() {
    let g = parse_graph("flowchart TD\n  A -->|\"`**yes**`\"| B\n  A -->|\"`__no__`\"| C").unwrap();
    assert_eq!(edge(&g, 0).label.as_deref(), Some("yes"));
    assert_eq!(edge(&g, 1).label.as_deref(), Some("no"));
}

#[test]
fn plain_label_keeps_literal_text_and_underscores() {
    // Not a markdown string (no backtick wrapper): Mermaid renders it literally, so brackets, snake_case, and any `*`/`_` must survive
    let g = parse_graph("flowchart TD\n  A[\"[ 464, 3797 ] seq_len d_model\"]").unwrap();
    assert_eq!(node(&g, 0).label, "[ 464, 3797 ] seq_len d_model");
}

#[test]
fn code_and_span_tags_are_stripped() {
    let g = parse_graph(
        "flowchart TD\n  A[\"<code>vocab_size</code> <span style=\\\"color:red\\\">x</span>\"]",
    )
    .unwrap();
    assert_eq!(node(&g, 0).label, "vocab_size x");
}

#[test]
fn bare_angle_brackets_are_kept() {
    let g = parse_graph("flowchart TD\n  A[\"a < b and c > d\"]").unwrap();
    assert_eq!(node(&g, 0).label, "a < b and c > d");
}

#[test]
fn generic_types_are_not_stripped_as_html() {
    // `<String>` / `<i32>` / `<id>` look like tags but are not HTML formatting tags, so they must survive
    // Only b/i/code/span/… etc. and <br> are stripped.
    let g =
        parse_graph("flowchart TD\n  A[\"Returns Vec<String>\"] --> B[\"Option<i32> for <id>\"]")
            .unwrap();
    assert_eq!(node(&g, 0).label, "Returns Vec<String>");
    assert_eq!(node(&g, 1).label, "Option<i32> for <id>");
}

#[test]
fn decode_html_entities_covers_named_numeric_and_double_escape() {
    assert_eq!(
        decode_html_entities("&lt;a&gt; &amp; &quot;x&quot; &apos;y&apos;"),
        "<a> & \"x\" 'y'"
    );
    assert_eq!(decode_html_entities("it&#39;s &#60;ok&#62;"), "it's <ok>");
    assert_eq!(
        decode_html_entities("&#x3c;tag&#X3E; &#x27;q&#x27;"),
        "<tag> 'q'"
    );
    // `&amp;lt;` must yield the literal `&lt;`, never `<`.
    assert_eq!(decode_html_entities("&amp;lt;"), "&lt;");
    assert_eq!(decode_html_entities("a &foo; b & c"), "a &foo; b & c");
    // Control chars (NUL collides with CONT, ESC injects ANSI) never decode.
    assert_eq!(decode_html_entities("a&#27;b&#0;c"), "a&#27;b&#0;c");
    assert_eq!(decode_html_entities("x&#x1b;y"), "x&#x1b;y");
}

#[test]
fn entity_escaped_flowchart_label_decodes_in_box_art() {
    let src = "flowchart LR\n  YAML[\"models-config/&lt;model&gt;/&lt;env&gt;.yaml\\nenterprise_api_config:\"]\n  PY[\"model_config_map.py\\nlanguage_model_dict_to_proto()\"]\n  YAML --> PY";
    let g = parse_graph(src).unwrap();
    assert!(
        node(&g, 0)
            .label
            .contains("models-config/<model>/<env>.yaml"),
        "{}",
        node(&g, 0).label
    );
    let art = plain(src);
    assert!(art.contains("<model>") && art.contains("<env>"), "{art}");
    assert!(!art.contains("&lt;") && !art.contains("&gt;"), "{art}");
}

#[test]
fn direct_push_sinks_decode_entities() {
    // Entities contain `;`, which split_statements treats as a separator, so they reach a sink intact only inside quotes
    // Assert through the real parsers where such quoting works
    let g = parse_state(
        "stateDiagram-v2\n  state \"work &lt;job&gt;\" as J\n  Idle --> Run: \"on &lt;go&gt;\"\n  Run: \"d &lt;e&gt;\"",
    )
    .unwrap();
    let node = |s: &str| g.nodes.iter().any(|n| n.label.contains(s));
    let edge = |s: &str| {
        g.edges
            .iter()
            .any(|e| e.label.as_deref().is_some_and(|l| l.contains(s)))
    };
    assert!(node("work <job>") && node("d <e>") && edge("on <go>"));
    assert!(!node("&lt;") && !edge("&lt;"));

    let (cg, _) = parse_class("classDiagram\n  A --> B : \"uses &lt;X&gt;\"").unwrap();
    assert!(cg.edges.iter().any(|e| {
        e.label
            .as_deref()
            .is_some_and(|l| l.contains("uses <X>") && !l.contains("&lt;"))
    }));

    let s = parse_sequence(
        "sequenceDiagram\n  A->>B: \"call &lt;svc&gt;\"\n  Note over A,B: \"memo &lt;o&gt;\"\n  alt \"c &lt;x&gt;\"\n    A->>B: ok\n  end",
    )
    .unwrap();
    assert!(
        s.items.iter().any(|it| matches!(it,
        SeqItem::Message { text: Some(t), .. } if t.contains("call <svc>") && !t.contains("&lt;")))
    );
    assert!(s.items.iter().any(|it| matches!(it,
        SeqItem::Note { text, .. } if text.contains("memo <o>") && !text.contains("&lt;"))));
    assert!(s.items.iter().any(|it| matches!(it,
        SeqItem::Divider { text } if text.contains("c <x>") && !text.contains("&lt;"))));

    // Class members and ER attributes have no clean quoted form (splitter fragments unquoted `;`; ER drops quoted text as a comment)
    // Exercise those decodes directly at push_member and push_er_attribute
    let mut member = ClassInfo::default();
    push_member(&mut member, "+run &lt;R&gt;");
    assert_eq!(member.attrs, vec!["+run <R>".to_string()]);
    let mut attr = ClassInfo::default();
    push_er_attribute(&mut attr, "string &lt;pk&gt;");
    assert_eq!(attr.attrs, vec!["string <pk>".to_string()]);
}

#[test]
fn quoted_label_with_inner_brackets_is_one_node() {
    let g = parse_graph(
        "flowchart TD\n  IDs[\"<b>Token IDs</b><br/>[ 464, 3797 ]<br/><i>indices</i>\"]",
    )
    .unwrap();
    assert_eq!(g.nodes.len(), 1, "inner brackets must not split the node");
    assert_eq!(g.edges.len(), 0, "no phantom edges from <br/> + brackets");
    assert_eq!(node(&g, 0).label, "Token IDs [ 464, 3797 ] indices");
}

#[test]
fn unquoted_label_with_embedded_quote_closes_at_bracket() {
    let g = parse_graph("flowchart TD\n  A[5\" pipe] --> B[24\" display]").unwrap();
    assert_eq!(g.nodes.len(), 2);
    assert_eq!(g.edges.len(), 1);
    assert_eq!(node(&g, 0).label, "5\" pipe");
    assert_eq!(node(&g, 1).label, "24\" display");
}

#[test]
fn quoted_label_with_inner_parens_is_one_node() {
    let g = parse_graph("flowchart TD\n  A[\"Tokenizer (BPE / WordPiece)\"] --> B[Done]").unwrap();
    assert_eq!(g.nodes.len(), 2);
    assert_eq!(g.edges.len(), 1);
    assert_eq!(node(&g, 0).label, "Tokenizer (BPE / WordPiece)");
}

#[test]
fn diagram_with_html_labels_renders_without_tag_artifacts() {
    let src = "flowchart TD\n  IDs[\"<b>3. Token IDs</b><br/>[ 464, 3797 ]<br/><i>indices</i>\"] --> Out[\"<b>done</b>\"]";
    let out = plain(src);
    assert!(!out.contains("<b>"), "raw HTML tag leaked:\n{out}");
    assert!(!out.contains("</"), "raw closing tag leaked:\n{out}");
    assert!(!out.contains("br/"), "phantom br artifact leaked:\n{out}");
    assert!(out.contains("Token IDs"), "label text missing:\n{out}");
}

#[test]
fn ranks_ignore_back_edges() {
    let g = parse_graph("graph TD\n A-->B\n B-->C\n C-->A").unwrap();
    let r = compute_ranks(&g);
    let idx = |id: &str| index_of(&g, id);
    assert_eq!(at(&r, idx("A")), 0);
    assert_eq!(at(&r, idx("B")), 1);
    assert_eq!(at(&r, idx("C")), 2);
}

#[test]
fn td_render_has_boxes_labels_and_arrow() {
    let out = plain("graph TD\n A[Start] --> B[End]");
    assert!(out.contains("Start"), "{out}");
    assert!(out.contains("End"), "{out}");
    assert!(out.contains('┌') || out.contains('╭'), "{out}");
    assert!(out.contains('▼'), "{out}");
}

#[test]
fn edge_label_is_rendered() {
    let out = plain("graph TD\n A-->|yes| B");
    assert!(out.contains("yes"), "{out}");
}

#[test]
fn lr_is_shorter_than_td_for_a_chain() {
    let chain = "A --> B --> C --> D";
    let td =
        plain_lines(&render(&format!("graph TD\n {chain}"), &styles(), Some(120)).unwrap()).len();
    let lr =
        plain_lines(&render(&format!("flowchart LR\n {chain}"), &styles(), Some(120)).unwrap())
            .len();
    assert!(lr < td, "expected LR ({lr}) shorter than TD ({td})");
}

#[test]
fn unsupported_diagram_uses_fallback_box() {
    let out = plain("gantt\n title Plan\n section A\n task :a1, 2024-01-01, 30d");
    assert!(out.contains("mermaid: gantt"), "{out}");
    assert!(out.contains("Plan"), "{out}");
}

#[test]
fn blank_source_returns_none() {
    assert!(render("   \n  ", &styles(), Some(80)).is_none());
}

#[test]
fn inline_label_with_x_or_o_letters() {
    let g = parse_graph("graph TD\n A -- no exit --> B").unwrap();
    assert_eq!(g.nodes.len(), 2);
    assert_eq!(g.edges.len(), 1);
    assert_eq!(edge(&g, 0).label.as_deref(), Some("no exit"));
}

#[test]
fn wide_glyph_box_stays_aligned() {
    let lines = plain_lines(&render("graph TD\n A[日本語ab]", &styles(), Some(120)).unwrap());
    let widths: Vec<usize> = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.width())
        .collect();
    assert!(
        widths.windows(2).all(|w| matches!(w, [a, b] if a == b)),
        "box rows must share one width: {widths:?}\n{lines:?}"
    );
    assert!(!lines.iter().any(|l| l.contains(CONT)), "sentinel leaked");
}

#[test]
fn merge_has_single_arrowhead() {
    let out = plain("graph TD\n A[aaa] --> D[ddddddd]\n B[bb] --> D\n C[ccccc] --> D");
    let arrows = out.chars().filter(|&c| c == '▼').count();
    assert_eq!(arrows, 1, "merge edges share one arrowhead:\n{out}");
    assert!(!out.contains("▼▼"), "must not stack arrowheads:\n{out}");
}

#[test]
fn long_label_wraps_without_truncation() {
    let out =
        plain("graph TD\n A[Check if the user has permission to access resource] --> B[Done]");
    assert!(out.contains("permission"), "{out}");
    assert!(out.contains("resource"), "{out}");
    assert!(!out.contains('…'), "should wrap, not truncate:\n{out}");
}

#[test]
fn very_long_label_truncates_after_max_lines() {
    let long = "alpha ".repeat(40);
    let out = plain(&format!("graph TD\n A[{}] --> B[x]", long.trim()));
    assert!(out.contains('…'), "should truncate past max lines:\n{out}");
}

#[test]
fn wrap_label_breaks_long_identifier_on_boundary() {
    let lines = wrap_label("mark_filter_restore_context", WRAP_WIDTH, MAX_LINES);
    // The first line ends on an identifier boundary, not a mid-segment slice.
    assert!(
        lines.first().is_some_and(|l| l.ends_with('_')),
        "first line must end on a boundary: {lines:?}"
    );
    // Every break (all but the last line) lands on a boundary char.
    let Some(prefix) = lines.len().checked_sub(1).and_then(|n| lines.get(..n)) else {
        panic!("expected wrapped lines, got {lines:?}");
    };
    for line in prefix {
        assert!(
            line.ends_with(LABEL_BREAK_CHARS),
            "line must break on a boundary: {line:?}"
        );
    }
    // Nothing is lost: the wrapped lines reconstruct the original word.
    assert_eq!(lines.concat(), "mark_filter_restore_context");
}

#[test]
fn wrap_label_token_without_break_char_falls_back_per_char() {
    let token = "a".repeat(40);
    let lines = wrap_label(&token, WRAP_WIDTH, MAX_LINES);
    // The token has no boundary char, so the wrap hard-breaks per char across multiple lines
    assert!(lines.len() >= 2, "must hard-break: {lines:?}");
    // 40 narrow chars fit in MAX_LINES or fewer lines, so nothing is truncated or lost
    assert_eq!(lines.concat(), token);
}

#[test]
fn flowchart_long_identifier_breaks_on_boundary_not_mid_segment() {
    let out = plain("graph TD\n A[mark_filter_restore_context] --> B[Done]");
    // The boundary-respecting pieces are present in the rendered art
    // `wrap_label_breaks_long_identifier_on_boundary` proves the break is on a boundary and lossless, so no exact-offset check here
    assert!(out.contains("mark_filter_restore_"), "{out}");
    assert!(out.contains("context"), "{out}");
}

#[test]
fn wrap_label_mixed_boundary_then_no_boundary_tail() {
    let token = String::from("ab_") + &"c".repeat(40);
    let lines = wrap_label(&token, WRAP_WIDTH, MAX_LINES);
    // The boundary is taken first ...
    assert!(
        lines.first().is_some_and(|l| l.ends_with('_')),
        "first break on boundary: {lines:?}"
    );
    // ... then the long no-boundary tail falls back to a per-char break.
    assert!(
        lines.iter().skip(1).any(|l| !l.contains(LABEL_BREAK_CHARS)),
        "a later line must be a per-char break: {lines:?}"
    );
    // 43 cols is less than MAX_LINES*WRAP_WIDTH, so it must not truncate; fully lossless
    assert_eq!(lines.concat(), token);
}

#[test]
fn wrap_label_boundary_breaking_still_truncates_at_max_lines() {
    let id = ["segment"; 20].join("_");
    let lines = wrap_label(&id, WRAP_WIDTH, MAX_LINES);
    // The identifier far exceeds MAX_LINES*WRAP_WIDTH, so it truncates ...
    assert_eq!(lines.len(), MAX_LINES);
    // ... with the ellipsis still on the final line.
    assert!(
        lines.last().unwrap().ends_with('…'),
        "truncation must keep the ellipsis: {lines:?}"
    );
}

#[test]
fn bt_flips_orientation() {
    let out = plain("flowchart BT\n A[first] --> B[second] --> C[third]");
    let lines: Vec<&str> = out.lines().collect();
    let row = |needle: &str| lines.iter().position(|l| l.contains(needle)).unwrap();
    assert!(
        row("third") < row("first"),
        "BT: 'third' should sit above 'first':\n{out}"
    );
}

#[test]
fn rl_flips_orientation() {
    let out = plain("flowchart RL\n A[first] --> B[second] --> C[third]");
    let line = out.lines().find(|l| l.contains("first")).unwrap();
    assert!(
        line.find("third") < line.find("first"),
        "RL: 'third' should sit left of 'first':\n{out}"
    );
}

#[test]
fn undirected_piped_label_has_no_arrowhead() {
    let out = plain("graph TD\n A ---|maybe| B");
    assert!(out.contains("maybe"), "{out}");
    assert!(
        !out.contains('▼'),
        "undirected link should not draw an arrow:\n{out}"
    );
}

#[test]
fn chain_edges_are_straight() {
    let out = plain("graph TD\n A[aaaa] --> B[b] --> C[cccccccc]");
    for line in out.lines() {
        assert!(
            !line.contains('└') || !line.contains('┐'),
            "chain should not jog: {line:?}"
        );
    }
}

#[test]
fn adversarial_chain_falls_back() {
    let mut src = String::from("graph TD\n");
    for i in 0..10_000 {
        src.push_str(" N");
        src.push_str(&i.to_string());
        src.push_str(" --> N");
        src.push_str(&(i + 1).to_string());
        src.push('\n');
    }
    let out = plain(&src);
    assert!(out.contains("mermaid: graph"), "expected fallback:\n{out}");
}

#[test]
fn single_statement_chain_over_cap_falls_back() {
    let mut src = String::from("graph LR\n ");
    for i in 0..10_000 {
        src.push('N');
        src.push_str(&i.to_string());
        src.push_str("-->");
    }
    src.push_str("N10000");
    let out = plain(&src);
    assert!(out.contains("mermaid: graph"), "expected fallback");
}

#[test]
fn deep_chain_within_caps_renders() {
    let mut src = String::from("graph TD\n");
    for i in 0..100 {
        src.push_str(" N");
        src.push_str(&i.to_string());
        src.push_str(" --> N");
        src.push_str(&(i + 1).to_string());
        src.push('\n');
    }
    let out = plain_lines(&render(&src, &styles(), Some(200)).unwrap());
    let joined = out.join("\n");
    assert!(joined.contains("N0"), "{joined}");
    assert!(joined.contains("N100"), "{joined}");
    assert!(joined.contains('▼'), "{joined}");
}

#[test]
fn fallback_styled_and_plain_widths_match() {
    let art = render("gantt\n title Plan\n a\n", &styles(), Some(120)).unwrap();
    assert_eq!(art.styled_lines.len(), plain_lines(&art).len());
    let Some(first) = plain_lines(&art).first().cloned() else {
        panic!("expected framed lines");
    };
    let frame_w = first.width();
    for (styled, plain) in art.styled_lines.iter().zip(&plain_lines(&art)) {
        let styled_w: usize = styled
            .spans
            .iter()
            .map(|s| s.content.as_ref().width())
            .sum();
        assert_eq!(styled_w, plain.width(), "styled/plain widths diverge");
        assert_eq!(plain.width(), frame_w, "fallback box must be rectangular");
    }
}

#[test]
fn over_wide_diagram_falls_back() {
    let src = "flowchart LR\n A[aaaaaaaaaaaaaaaaaaaa] --> B[bbbbbbbbbbbbbbbbbbbb] --> C[cccccccccccccccccccc]";
    let out = plain_lines(&render(src, &styles(), Some(40)).unwrap());
    let joined = out.join("\n");
    assert!(
        joined.contains("mermaid: flowchart"),
        "expected fallback for over-wide diagram:\n{joined}"
    );
    let max_w = out.iter().map(|l| l.width()).max().unwrap_or(0);
    let fits = plain_lines(&render(src, &styles(), Some(120)).unwrap());
    assert!(
        fits.iter().any(|l| l.contains('▶')),
        "same diagram should render when it fits"
    );
    assert!(max_w <= src.len(), "fallback width bounded by source");
}

#[test]
fn too_wide_fallback_appends_hint_below_box() {
    let src = "flowchart LR\n A[aaaaaaaaaaaaaaaaaaaa] --> B[bbbbbbbbbbbbbbbbbbbb] --> C[cccccccccccccccccccc]";
    let out = plain_lines(&render(src, &styles(), Some(40)).unwrap());
    let joined = out.join("\n");

    assert!(
        joined.contains("mermaid: flowchart"),
        "plain header:\n{joined}"
    );
    assert!(
        !joined.contains("(too wide)"),
        "header stays plain:\n{joined}"
    );
    assert!(
        joined.contains("flowchart LR"),
        "raw source kept:\n{joined}"
    );

    let bottom = out
        .iter()
        .position(|l| l.contains('╰'))
        .expect("box bottom");
    let note = out
        .iter()
        .position(|l| l.contains("too wide"))
        .expect("note row");
    assert!(note > bottom, "note must be below the box:\n{joined}");
    assert!(
        joined.contains("open the image"),
        "note points at the image:\n{joined}"
    );

    assert!(
        out.iter().all(|l| l.width() <= 40),
        "fits 40 cols:\n{joined}"
    );
}

#[test]
fn unsupported_diagram_fallback_not_flagged_too_wide() {
    let out = plain("gantt\n title Plan\n section A\n task :a1, 2024-01-01, 30d");
    assert!(out.contains("mermaid: gantt"), "{out}");
    assert!(
        !out.contains("too wide"),
        "unsupported type is not a width problem:\n{out}"
    );
}

#[test]
fn fitting_diagram_has_no_width_warning() {
    let out = plain("flowchart LR\n A[Start] --> B[End]");
    assert!(
        !out.contains("too wide"),
        "fitting diagram must not warn:\n{out}"
    );
    assert!(
        !out.contains("mermaid: flowchart"),
        "should draw art, not box:\n{out}"
    );
    assert!(out.contains('▶'), "should draw edges:\n{out}");
}

#[test]
fn bidirectional_link_draws_both_arrowheads() {
    let lr = plain("flowchart LR\n A <--> B");
    assert!(lr.contains('◄') && lr.contains('▶'), "{lr}");
    let td = plain("graph TD\n A <--> B");
    assert!(td.contains('▲') && td.contains('▼'), "{td}");
}

#[test]
fn reversed_arrow_swaps_edge_direction() {
    let g = parse_graph("graph TD\n A <-- B").unwrap();
    let idx = |id: &str| index_of(&g, id);
    assert_eq!(g.edges.len(), 1);
    assert_eq!(edge(&g, 0).from, idx("B"));
    assert_eq!(edge(&g, 0).to, idx("A"));
    assert_eq!(edge(&g, 0).head_to, Head::Arrow);
    assert_eq!(edge(&g, 0).head_from, Head::None);
    let out = plain("graph TD\n A <-- B");
    let lines: Vec<&str> = out.lines().collect();
    let row = |needle: &str| lines.iter().position(|l| l.contains(needle)).unwrap();
    assert!(row("B") < row("A"), "B should rank above A:\n{out}");
}

#[test]
fn semicolon_and_comment_survive_inside_quoted_label() {
    let g = parse_graph("graph TD\n A[\"wait; 50%% done\"] --> B").unwrap();
    assert_eq!(g.nodes.len(), 2);
    assert_eq!(node(&g, 0).label, "wait; 50%% done");
}

#[test]
fn comment_outside_quotes_is_stripped() {
    let g = parse_graph("graph TD %% main flow\n A --> B %% trailing\n %% full line\n").unwrap();
    assert_eq!(g.nodes.len(), 2);
    assert_eq!(g.edges.len(), 1);
}

/// An edge that skips a rank is a tag pair, so no line passes the box in between.
#[test]
fn skip_edge_is_a_tag_pair_not_a_line_past_boxes() {
    assert_eq!(
        plain("graph TD\n A --> B\n B --> C\n A --> C"),
        [
            "     ┌───┐",
            "     │ A ├(a)",
            "     └─┬─┘",
            "       │",
            "       ▼",
            "     ┌───┐",
            "     │ B │",
            "     └─┬─┘",
            "       │",
            "       ▼",
            "     ┌───┐",
            " (a)▶│ C │",
            "     └───┘",
        ]
        .join("\n")
    );
}

fn ordered_ranks(src: &str) -> (Graph, Vec<usize>, Vec<Vec<usize>>) {
    let g = parse_graph(src).unwrap();
    let ranks = compute_ranks(&g);
    let max_rank = *ranks.iter().max().unwrap();
    let mut by_rank: Vec<Vec<usize>> = vec![Vec::new(); max_rank + 1];
    for (idx, &r) in ranks.iter().enumerate() {
        if let Some(row) = by_rank.get_mut(r) {
            row.push(idx);
        }
    }
    order_ranks(&mut by_rank, &g.edges, &ranks);
    (g, ranks, by_rank)
}

#[test]
fn order_ranks_removes_avoidable_crossing() {
    let (g, ranks, by_rank) = ordered_ranks("graph TD\n C[ccc]\n D[ddd]\n A --> D\n B --> C");
    let mut pos = vec![0usize; g.nodes.len()];
    for row in &by_rank {
        for (i, &v) in row.iter().enumerate() {
            if let Some(slot) = pos.get_mut(v) {
                *slot = i;
            }
        }
    }
    assert_eq!(count_crossings(&g.edges, &ranks, &pos), 0);
    let idx = |id: &str| index_of(&g, id);
    assert!(
        at(&pos, idx("D")) < at(&pos, idx("C")),
        "D follows parent A leftward"
    );
}

#[test]
fn order_ranks_keeps_crossing_free_order() {
    let (g, ranks, by_rank) = ordered_ranks("graph TD\n A --> C\n B --> D");
    let idx = |id: &str| index_of(&g, id);
    assert_eq!(by_rank.first(), Some(&vec![idx("A"), idx("B")]));
    assert_eq!(by_rank.get(1), Some(&vec![idx("C"), idx("D")]));
    let mut pos = vec![0usize; g.nodes.len()];
    for row in &by_rank {
        for (i, &v) in row.iter().enumerate() {
            if let Some(slot) = pos.get_mut(v) {
                *slot = i;
            }
        }
    }
    assert_eq!(count_crossings(&g.edges, &ranks, &pos), 0);
}

#[test]
fn crossing_edges_render_untangled() {
    let out = plain("graph TD\n C[ccc]\n D[ddd]\n A --> D\n B --> C");
    let row = out
        .lines()
        .find(|l| l.contains("ccc") && l.contains("ddd"))
        .unwrap();
    assert!(
        row.find("ddd") < row.find("ccc"),
        "children reorder under their parents:\n{out}"
    );
    assert!(!out.contains('┼'), "{out}");
}

#[test]
fn three_layer_weave_untangles() {
    let (g, ranks, by_rank) = ordered_ranks(
        "graph TD\n X[x]\n Y[y]\n A --> Y\n B --> X\n X --> Q\n Y --> P\n P[p]\n Q[q]",
    );
    let mut pos = vec![0usize; g.nodes.len()];
    for row in &by_rank {
        for (i, &v) in row.iter().enumerate() {
            if let Some(slot) = pos.get_mut(v) {
                *slot = i;
            }
        }
    }
    assert_eq!(
        count_crossings(&g.edges, &ranks, &pos),
        0,
        "both layers untangle"
    );
}

#[test]
fn unavoidable_crossing_gets_separate_bus_rows() {
    let crossing = plain("graph TD\n A --> D[ddd]\n A --> C[ccc]\n B --> C\n B --> D");
    let parallel = plain("graph TD\n A --> C[ccc]\n B --> D[ddd]");
    assert!(crossing.contains('┼'), "wire crossing renders:\n{crossing}");
    assert_eq!(
        crossing.lines().count(),
        parallel.lines().count() + 1,
        "crossing pair claims one extra bus row:\n{crossing}"
    );
    assert_eq!(
        crossing.chars().filter(|&c| c == '▼').count(),
        2,
        "{crossing}"
    );
}

#[test]
fn fan_out_keeps_single_bus_row() {
    let out = plain("graph TD\n A --> C[ccc]\n A --> D[ddd]");
    let baseline = plain("graph TD\n A --> C[ccc]");
    assert_eq!(
        out.lines().count(),
        baseline.lines().count(),
        "shared-source jogs share one bus row:\n{out}"
    );
    assert!(!out.contains('┼'), "{out}");
}

/// Tagged edges into one box share a single arrow head.
#[test]
fn shared_target_tags_share_one_arrow() {
    assert_eq!(
        plain("graph TD\n A --> B\n B --> C\n B --> A\n C --> A"),
        [
            "        ┌───┐",
            " (a)(b)▶│ A │",
            "        └─┬─┘",
            "          │",
            "          ▼",
            "        ┌───┐",
            "        │ B ├(a)",
            "        └─┬─┘",
            "          │",
            "          ▼",
            "        ┌───┐",
            "        │ C ├(b)",
            "        └───┘",
        ]
        .join("\n")
    );
}

/// Back edges are a matched tag pair beside their two boxes, not a run to a lane.
#[test]
fn back_edges_become_tag_pairs() {
    assert_eq!(
        plain("graph TD\n A --> B\n B --> C\n B --> A\n C --> B"),
        [
            "     ┌───┐",
            " (a)▶│ A │",
            "     └─┬─┘",
            "       │",
            "       ▼",
            "     ┌───┐",
            " (b)▶│ B ├(a)",
            "     └─┬─┘",
            "       │",
            "       ▼",
            "     ┌───┐",
            "     │ C ├(b)",
            "     └───┘",
        ]
        .join("\n")
    );
}

#[test]
fn fallback_wraps_long_lines_to_max_width() {
    let out = plain_lines(
        &render(
            "gantt\n title a very long line that should wrap inside the fallback box nicely",
            &styles(),
            Some(40),
        )
        .unwrap(),
    );
    assert!(out.iter().all(|l| l.width() <= 40), "{}", out.join("\n"));
    let Some(mid) = out.len().checked_sub(1).and_then(|n| out.get(1..n)) else {
        panic!("expected boxed output, got {out:?}");
    };
    for line in mid {
        assert!(
            line.starts_with('│') && line.ends_with('│'),
            "body rows keep both borders: {line:?}"
        );
    }
    assert!(out.join("\n").contains("nicely"), "{}", out.join("\n"));
}

#[test]
fn class_renders_compartments() {
    let out =
        plain("classDiagram\n class Animal {\n +int age\n +isMammal() bool\n }\n Animal <|-- Duck");
    assert!(out.contains("Animal"), "{out}");
    assert!(out.contains("+int age"), "{out}");
    assert!(out.contains("+isMammal() bool"), "{out}");
    assert!(
        out.contains('├') && out.contains('┤'),
        "section rules:\n{out}"
    );
    let lines: Vec<&str> = out.lines().collect();
    let name = lines.iter().position(|l| l.contains("Animal")).unwrap();
    let attr = lines.iter().position(|l| l.contains("+int age")).unwrap();
    let method = lines
        .iter()
        .position(|l| l.contains("+isMammal() bool"))
        .unwrap();
    assert!(name < attr && attr < method, "{out}");
}

#[test]
fn class_inheritance_triangle_at_parent() {
    let out = plain("classDiagram\n Animal <|-- Duck\n Animal <|-- Fish");
    assert!(out.contains('△'), "hollow triangle:\n{out}");
    let lines: Vec<&str> = out.lines().collect();
    let animal = lines.iter().position(|l| l.contains("Animal")).unwrap();
    let duck = lines.iter().position(|l| l.contains("Duck")).unwrap();
    assert!(animal < duck, "parent above child:\n{out}");
    let tri = lines.iter().position(|l| l.contains('△')).unwrap();
    assert!(
        tri >= animal && tri < duck,
        "triangle at parent end:\n{out}"
    );
}

#[test]
fn class_realization_is_dotted_triangle() {
    let g = parse_class("classDiagram\n IShape <|.. Circle").unwrap().0;
    assert_eq!(edge(&g, 0).head_from, Head::Triangle);
    assert!(edge(&g, 0).line == LineKind::Dotted);
    let out = plain("classDiagram\n IShape <|.. Circle");
    assert!(out.contains('╎') || out.contains('╌'), "{out}");
}

#[test]
fn class_composition_and_aggregation_diamonds() {
    let out = plain("classDiagram\n Car *-- Engine\n Pond o-- Duck");
    assert!(out.contains('◆'), "filled diamond:\n{out}");
    assert!(out.contains('◇'), "open diamond:\n{out}");
}

#[test]
fn class_dependency_dotted_arrow() {
    let g = parse_class("classDiagram\n A ..> B").unwrap().0;
    assert_eq!(edge(&g, 0).head_to, Head::Arrow);
    assert!(edge(&g, 0).line == LineKind::Dotted);
}

#[test]
fn class_colon_members_merge_with_block() {
    let out =
        plain("classDiagram\n class Duck {\n +swim()\n }\n Duck : +String beakColor\n S --> Duck");
    assert!(out.contains("+swim()"), "{out}");
    assert!(out.contains("+String beakColor"), "{out}");
}

#[test]
fn class_annotation_renders_guillemets() {
    let out = plain("classDiagram\n <<interface>> Shape\n Shape <|.. Circle");
    assert!(out.contains("«interface»"), "{out}");
}

#[test]
fn class_generics_display_as_angle_brackets() {
    let out = plain("classDiagram\n Shape~T~ : +area() T\n S --> Shape~T~");
    assert!(out.contains("Shape<T>"), "{out}");
    assert!(!out.contains('~'), "{out}");
}

#[test]
fn class_cardinalities_fold_into_label() {
    let out = plain("classDiagram\n Student \"many\" --> \"1\" School : attends");
    assert!(out.contains("many attends 1"), "{out}");
}

#[test]
fn class_from_end_head_survives_fan_out_jog() {
    let out = plain("classDiagram\n Animal <|-- Duck\n Animal <|-- Fish\n Animal <|-- Cow");
    assert_eq!(
        out.matches('△').count() + out.matches('▽').count(),
        1,
        "merged from-end glyph on the parent border:\n{out}"
    );
}

#[test]
fn class_empty_class_is_plain_titled_box() {
    let out = plain("classDiagram\n class Loner\n A --> Loner");
    assert!(out.contains("Loner"), "{out}");
}

#[test]
fn class_unknown_statement_falls_back() {
    let out = plain("classDiagram\n A --> B\n total garbage here");
    assert!(out.contains("mermaid: classDiagram"), "{out}");
}

#[test]
fn class_member_cap_ellipsis() {
    let mut src = String::from("classDiagram\n class Big {\n");
    for i in 0..12 {
        src.push_str(" +field");
        src.push_str(&i.to_string());
        src.push('\n');
    }
    src.push_str(" }\n A --> Big");
    let out = plain(&src);
    assert!(out.contains("+field7"), "{out}");
    assert!(!out.contains("+field9"), "{out}");
    assert!(out.contains('…'), "{out}");
}

#[test]
fn class_direction_lr() {
    let out = plain("classDiagram\n direction LR\n A --> B");
    let line = out.lines().find(|l| l.contains('A')).unwrap();
    assert!(line.contains('B'), "{out}");
}

#[test]
fn er_renders_entities_and_relationship_labels() {
    let out = plain(
        "erDiagram\n CUSTOMER ||--o{ ORDER : places\n CUSTOMER {\n string name PK \"full name\"\n int custNumber\n }",
    );
    assert!(out.contains("CUSTOMER"), "{out}");
    assert!(out.contains("ORDER"), "{out}");
    assert!(out.contains("string name PK"), "{out}");
    assert!(
        !out.contains("full name"),
        "attribute comments dropped:\n{out}"
    );
    assert!(out.contains("1 places 0..*"), "{out}");
    assert!(out.contains('├'), "attribute compartment rule:\n{out}");
}

#[test]
fn er_cardinality_map() {
    let cases = [
        ("||--||", "1", "1"),
        ("|o--o|", "0..1", "0..1"),
        ("}o--o{", "0..*", "0..*"),
        ("}|--|{", "1..*", "1..*"),
        ("||--o{", "1", "0..*"),
    ];
    for (op, l, r) in cases {
        let (cl, cr, line) = parse_er_op(op).unwrap();
        assert_eq!((cl, cr), (l, r), "{op}");
        assert!(line == LineKind::Solid);
    }
    assert!(parse_er_op("||..o{").unwrap().2 == LineKind::Dotted);
    assert!(parse_er_op("||==o{").is_none());
    assert!(parse_er_op("garbage").is_none());
}

#[test]
fn er_non_identifying_renders_dotted() {
    let out = plain("erDiagram\n A ||..o{ B : uses");
    assert!(out.contains('╎') || out.contains('╌'), "{out}");
}

#[test]
fn er_relationships_have_no_arrowheads() {
    let out = plain("erDiagram\n A ||--o{ B : has");
    for head in ['▼', '▲', '◄', '▶', '△', '◆', '◇'] {
        assert!(!out.contains(head), "{head} in:\n{out}");
    }
}

#[test]
fn er_entity_alias_label() {
    let out = plain("erDiagram\n p[Person] ||--o{ a[\"Bank Account\"] : owns");
    assert!(out.contains("Person"), "{out}");
    assert!(out.contains("Bank Account"), "{out}");
}

#[test]
fn er_unquoted_label_and_bare_entity_decl() {
    let g = parse_er("erDiagram\n LONER\n A ||--|| B : linked")
        .unwrap()
        .0;
    assert_eq!(g.nodes.len(), 3);
    let out = plain("erDiagram\n LONER\n A ||--|| B : linked");
    assert!(out.contains("LONER"), "{out}");
    assert!(out.contains("1 linked 1"), "{out}");
}

#[test]
fn er_attribute_cap_ellipsis() {
    let mut src = String::from("erDiagram\n BIG {\n");
    for i in 0..12 {
        src.push_str(" int f");
        src.push_str(&i.to_string());
        src.push('\n');
    }
    src.push_str(" }\n BIG ||--|| OTHER : x");
    let out = plain(&src);
    assert!(out.contains("int f7"), "{out}");
    assert!(!out.contains("int f9"), "{out}");
    assert!(out.contains('…'), "{out}");
}

#[test]
fn er_unknown_statement_falls_back() {
    let out = plain("erDiagram\n A ||--|| B : ok\n utter nonsense statement");
    assert!(out.contains("mermaid: erDiagram"), "{out}");
}

#[test]
fn subgraph_renders_titled_frame() {
    let out = plain(
        "graph TD\n S[Start] --> one\n subgraph one [Group One]\n A --> B\n end\n one --> E[End]",
    );
    assert!(out.contains(" Group One "), "{out}");
    let lines: Vec<&str> = out.lines().collect();
    let title = lines.iter().position(|l| l.contains("Group One")).unwrap();
    let a = lines.iter().position(|l| l.contains("│ A │")).unwrap();
    let b = lines.iter().position(|l| l.contains("│ B │")).unwrap();
    let frame_close = lines
        .iter()
        .rposition(|l| l.trim_start().starts_with('└'))
        .unwrap();
    assert!(title < a && a < b && b <= frame_close, "{out}");
    assert!(out.contains("Start") && out.contains("End"), "{out}");
    assert_eq!(out.matches('▼').count(), 3, "{out}");
}

#[test]
fn subgraph_edge_between_groups() {
    let out = plain(
        "graph TD\n subgraph api [API]\n A1 --> A2\n end\n subgraph db [Storage]\n B1\n end\n api --> db",
    );
    assert!(out.contains(" API "), "{out}");
    assert!(out.contains(" Storage "), "{out}");
    let lines: Vec<&str> = out.lines().collect();
    let api = lines.iter().position(|l| l.contains("API")).unwrap();
    let db = lines.iter().position(|l| l.contains("Storage")).unwrap();
    assert!(api < db, "API frame ranks above Storage:\n{out}");
}

#[test]
fn subgraph_nested_frames() {
    let out = plain(
        "graph TD\n subgraph outer [Outer]\n subgraph inner [Inner]\n X --> Y\n end\n W --> X\n end\n S --> outer",
    );
    assert!(out.contains(" Outer "), "{out}");
    assert!(out.contains(" Inner "), "{out}");
    let lines: Vec<&str> = out.lines().collect();
    let outer = lines.iter().position(|l| l.contains("Outer")).unwrap();
    let inner = lines.iter().position(|l| l.contains("Inner")).unwrap();
    assert!(outer < inner, "{out}");
}

#[test]
fn subgraph_cross_member_edge_attaches_to_frame() {
    let out = plain("graph LR\n S --> A\n subgraph g [Workers]\n A --> B\n end\n B --> T");
    assert!(out.contains(" Workers "), "{out}");
    assert!(out.contains('S') && out.contains('T'), "{out}");
    assert_eq!(out.matches('▶').count(), 3, "{out}");
    let row = out.lines().find(|l| l.contains("│ A ├")).unwrap();
    assert!(
        row.find('S') < row.find('A'),
        "A stays outside the group (first definition wins):\n{out}"
    );
}

#[test]
fn subgraph_id_referenced_before_declaration() {
    let g = parse_graph("graph TD\n X --> two\n subgraph two\n C --> D\n end").unwrap();
    assert_eq!(g.groups.len(), 1);
    let out = plain("graph TD\n X --> two\n subgraph two\n C --> D\n end");
    assert!(out.contains(" two "), "frame titled by id:\n{out}");
    assert!(out.contains("│ C │"), "{out}");
}

#[test]
fn subgraph_quoted_and_plain_titles() {
    let out = plain("graph TD\n subgraph \"My Stuff\"\n A\n end\n S --> A");
    assert!(out.contains(" My Stuff "), "{out}");
    let out2 = plain("graph TD\n subgraph batch jobs\n B\n end\n S --> B");
    assert!(out2.contains(" batch jobs "), "{out2}");
    let out3 = plain("graph TD\n subgraph \"a &lt;b&gt;\"\n C\n end\n S --> C");
    assert!(out3.contains("a <b>") && !out3.contains("&lt;"), "{out3}");
}

#[test]
fn subgraph_empty_is_dropped() {
    let out = plain("graph TD\n subgraph ghost\n end\n A --> B");
    assert!(!out.contains("ghost"), "{out}");
    assert!(out.contains('▼'), "{out}");
}

#[test]
fn subgraph_bt_flips_frame_and_contents() {
    let out = plain("flowchart BT\n S --> one\n subgraph one [Up]\n A --> B\n end");
    assert!(out.contains(" Up "), "{out}");
    let lines: Vec<&str> = out.lines().collect();
    let row = |needle: &str| lines.iter().position(|l| l.contains(needle)).unwrap();
    assert!(row("│ B │") < row("│ A │"), "contents flip with BT:\n{out}");
    assert!(row(" Up ") < row("S"), "frame above source in BT:\n{out}");
    assert!(out.contains('▲'), "{out}");
}

#[test]
fn subgraph_depth_over_cap_falls_back() {
    let mut src = String::from("graph TD\n");
    for i in 0..8 {
        src.push_str(" subgraph g");
        src.push_str(&i.to_string());
        src.push('\n');
    }
    src.push_str(" A --> B\n");
    for _ in 0..8 {
        src.push_str(" end\n");
    }
    let out = plain(&src);
    assert!(out.contains("mermaid: graph"), "{out}");
}

#[test]
fn subgraph_groupless_path_unchanged() {
    let g = parse_graph("graph TD\n A --> B").unwrap();
    assert!(g.groups.is_empty());
}

#[test]
fn fan_out_creates_cross_product_edges() {
    let g = parse_graph("graph TD\n A & B --> C & D").unwrap();
    assert_eq!(g.nodes.len(), 4);
    assert_eq!(g.edges.len(), 4);
    let idx = |id: &str| index_of(&g, id);
    let has = |f: &str, t: &str| g.edges.iter().any(|e| e.from == idx(f) && e.to == idx(t));
    assert!(has("A", "C") && has("A", "D") && has("B", "C") && has("B", "D"));
    let out = plain("graph TD\n A & B --> C & D");
    assert_eq!(out.chars().filter(|&c| c == '▼').count(), 2, "{out}");
}

#[test]
fn fan_out_in_chain() {
    let g = parse_graph("graph LR\n A & B --> C --> D").unwrap();
    assert_eq!(g.edges.len(), 3);
}

#[test]
fn fan_out_with_reversed_arrow() {
    let g = parse_graph("graph TD\n A & B <-- C").unwrap();
    let idx = |id: &str| index_of(&g, id);
    assert_eq!(g.edges.len(), 2);
    assert!(g.edges.iter().all(|e| e.from == idx("C")));
    assert!(g.edges.iter().all(|e| e.head_to == Head::Arrow));
}

#[test]
fn circle_and_cross_endings_create_no_phantom_nodes() {
    let g = parse_graph("graph TD\n A --o B\n C --x D").unwrap();
    assert_eq!(g.nodes.len(), 4, "no phantom o/x nodes");
    assert!(!g.index.contains_key("o"));
    assert!(!g.index.contains_key("x"));
    assert_eq!(edge(&g, 0).head_to, Head::Circle);
    assert_eq!(edge(&g, 1).head_to, Head::Cross);
    let out = plain("graph TD\n A --o B");
    assert!(out.contains('o'), "circle head rendered:\n{out}");
}

#[test]
fn left_endings_decorate_without_reversing() {
    let g = parse_graph("graph TD\n A o-- B\n C x-- D").unwrap();
    let idx = |id: &str| index_of(&g, id);
    assert_eq!(edge(&g, 0).from, idx("A"));
    assert_eq!(edge(&g, 0).to, idx("B"));
    assert_eq!(edge(&g, 0).head_from, Head::Circle);
    assert_eq!(edge(&g, 1).head_from, Head::Cross);
    assert_eq!(edge(&g, 0).head_to, Head::None);
}

#[test]
fn reversed_arrow_with_end_marker_swaps_direction() {
    let g = parse_graph("graph TD\n A <--o B\n C <--x D").unwrap();
    let idx = |id: &str| index_of(&g, id);
    assert_eq!(edge(&g, 0).from, idx("B"));
    assert_eq!(edge(&g, 0).to, idx("A"));
    assert_eq!(edge(&g, 0).head_to, Head::Arrow);
    assert_eq!(edge(&g, 0).head_from, Head::Circle);
    assert_eq!(edge(&g, 1).from, idx("D"));
    assert_eq!(edge(&g, 1).to, idx("C"));
    assert_eq!(edge(&g, 1).head_from, Head::Cross);
    let plain_rev = plain("graph TD\n A <--o B");
    let lines: Vec<&str> = plain_rev.lines().collect();
    let row = |needle: &str| lines.iter().position(|l| l.contains(needle)).unwrap();
    assert!(
        row("B") < row("A"),
        "ranks match plain <-- reversal:\n{plain_rev}"
    );
}

#[test]
fn both_end_markers_parse() {
    let g = parse_graph("graph TD\n A o--o B\n C x--x D").unwrap();
    assert_eq!(edge(&g, 0).head_from, Head::Circle);
    assert_eq!(edge(&g, 0).head_to, Head::Circle);
    assert_eq!(edge(&g, 1).head_from, Head::Cross);
    assert_eq!(edge(&g, 1).head_to, Head::Cross);
    assert_eq!(g.nodes.len(), 4);
}

#[test]
fn dotted_and_thick_lines_render_distinctly() {
    let dotted = plain("graph TD\n A -.-> B");
    assert!(dotted.contains('╎'), "dotted vertical:\n{dotted}");
    let thick = plain("graph TD\n A ==> B");
    assert!(thick.contains('┃'), "thick vertical:\n{thick}");
    let solid = plain("graph TD\n A --> B");
    assert!(
        !solid.contains('╎') && !solid.contains('┃'),
        "solid unchanged:\n{solid}"
    );
}

#[test]
fn dotted_label_form_renders_dashed() {
    let out = plain("graph LR\n A -. maybe .-> B");
    assert!(out.contains('╌'), "{out}");
    assert!(out.contains("maybe"), "{out}");
}

#[test]
fn thick_jog_uses_thick_corners() {
    let out = plain("graph TD\n A[aaaaaaa] ==> B\n A ==> C[ccccccc]");
    assert!(
        out.contains('┏') || out.contains('┓') || out.contains('┳'),
        "thick corners on jog:\n{out}"
    );
}

#[test]
fn state_diagram_renders_states_and_transitions() {
    let out = plain("stateDiagram-v2\n [*] --> Idle\n Idle --> Running: start\n Running --> [*]");
    assert!(out.contains("Idle"), "{out}");
    assert!(out.contains("Running"), "{out}");
    assert!(out.contains("start"), "{out}");
    assert!(out.contains('▼'), "{out}");
    assert_eq!(
        out.matches('●').count(),
        2,
        "distinct start and end markers:\n{out}"
    );
    let lines: Vec<&str> = out.lines().collect();
    let first_dot = lines.iter().position(|l| l.contains('●')).unwrap();
    let last_dot = lines.iter().rposition(|l| l.contains('●')).unwrap();
    let idle = lines.iter().position(|l| l.contains("Idle")).unwrap();
    assert!(first_dot < idle && idle < last_dot, "{out}");
}

#[test]
fn state_v1_header_renders() {
    let out = plain("stateDiagram\n A --> B");
    assert!(out.contains('▼'), "{out}");
}

#[test]
fn state_boxes_are_rounded() {
    let out = plain("stateDiagram-v2\n A --> B");
    assert!(out.contains('╭'), "{out}");
    assert!(!out.contains('┌'), "states render rounded:\n{out}");
}

#[test]
fn state_alias_label_renders() {
    let out = plain("stateDiagram-v2\n state \"Waiting for input\" as W\n W --> Done");
    assert!(out.contains("Waiting for input"), "{out}");
}

#[test]
fn state_choice_parses_as_diamond() {
    let g =
        parse_state("stateDiagram-v2\n state c <<choice>>\n A --> c\n c --> B: yes\n c --> D: no")
            .unwrap();
    assert!(node(&g, index_of(&g, "c")).shape == Shape::Diamond);
    assert_eq!(g.edges.len(), 3);
}

#[test]
fn state_description_sets_label() {
    let out = plain("stateDiagram-v2\n s2 : waits patiently\n A --> s2");
    assert!(out.contains("waits patiently"), "{out}");
}

#[test]
fn state_direction_lr() {
    let out = plain("stateDiagram-v2\n direction LR\n A --> B --> C");
    let td = plain("stateDiagram-v2\n A --> B");
    assert!(
        out.lines().count() <= td.lines().count() + 2,
        "LR stays flat:\n{out}"
    );
    let line = out.lines().find(|l| l.contains('A')).unwrap();
    assert!(line.contains('B'), "A and B share a row in LR:\n{out}");
}

#[test]
fn state_composite_contents_render_flat() {
    let out = plain("stateDiagram-v2\n state Active {\n A --> B\n }\n Active --> Done");
    assert!(out.contains("Active"), "{out}");
    assert!(out.contains('A') && out.contains('B'), "{out}");
    assert!(out.contains("Done"), "{out}");
}

#[test]
fn state_notes_are_skipped() {
    let out = plain(
        "stateDiagram-v2\n A --> B\n note right of A: inline note\n note left of B\n block text\n end note",
    );
    assert!(out.contains('▼'), "{out}");
    assert!(!out.contains("note"), "{out}");
    assert!(!out.contains("block text"), "{out}");
}

/// A back transition's label is drawn inside its target tag.
#[test]
fn state_back_transition_label_rides_in_the_tag() {
    assert_eq!(
        plain("stateDiagram-v2\n A --> B\n B --> C\n C --> B: retry"),
        [
            "            ╭───╮",
            "            │ A │",
            "            ╰─┬─╯",
            "              │",
            "              ▼",
            "            ╭───╮",
            " (a: retry)▶│ B │",
            "            ╰─┬─╯",
            "              │",
            "              ▼",
            "            ╭───╮",
            "            │ C ├(a)",
            "            ╰───╯",
        ]
        .join("\n")
    );
}

#[test]
fn state_unknown_statement_falls_back() {
    let out = plain("stateDiagram-v2\n A --> B\n some garbage line");
    assert!(out.contains("mermaid: stateDiagram-v2"), "{out}");
}

#[test]
fn state_over_cap_falls_back() {
    let mut src = String::from("stateDiagram-v2\n");
    for i in 0..600 {
        src.push_str(" S");
        src.push_str(&i.to_string());
        src.push_str(" --> S");
        src.push_str(&(i + 1).to_string());
        src.push('\n');
    }
    let out = plain(&src);
    assert!(out.contains("mermaid: stateDiagram-v2"), "{out}");
}

#[test]
fn state_extra_dash_arrow_tolerated() {
    let g = parse_state("stateDiagram-v2\n A ---> B").unwrap();
    assert_eq!(g.edges.len(), 1);
    assert_eq!(g.nodes.len(), 2);
}

#[test]
fn state_description_preserves_choice_shape() {
    let g =
        parse_state("stateDiagram-v2\n state c <<choice>>\n c : pick a path\n A --> c\n c --> B")
            .unwrap();
    assert!(node(&g, index_of(&g, "c")).shape == Shape::Diamond);
    assert_eq!(node(&g, index_of(&g, "c")).label, "pick a path");
    let g2 = parse_state("stateDiagram-v2\n state c <<choice>>\n state \"pick\" as c\n A --> c")
        .unwrap();
    assert!(node(&g2, index_of(&g2, "c")).shape == Shape::Diamond);
    assert_eq!(node(&g2, index_of(&g2, "c")).label, "pick");
}

#[test]
fn state_chained_transitions_parse_as_separate_edges() {
    let g = parse_state("stateDiagram-v2\n A --> B --> C").unwrap();
    assert_eq!(g.nodes.len(), 3, "three distinct states");
    assert_eq!(g.edges.len(), 2, "two edges");
    assert!(g.index.contains_key("B"));
    assert!(g.index.contains_key("C"));
    assert!(
        !g.nodes.iter().any(|n| n.label.contains("-->")),
        "no node swallows the arrow"
    );
    let idx = |id: &str| index_of(&g, id);
    assert!(g
        .edges
        .iter()
        .any(|e| e.from == idx("A") && e.to == idx("B")));
    assert!(g
        .edges
        .iter()
        .any(|e| e.from == idx("B") && e.to == idx("C")));
}

#[test]
fn state_chain_with_markers_and_label() {
    let g = parse_state("stateDiagram-v2\n [*] --> A --> B: done").unwrap();
    assert_eq!(g.edges.len(), 2);
    assert!(g.edges.iter().any(|e| e.label.as_deref() == Some("done")));
    let out = plain("stateDiagram-v2\n [*] --> A --> B: done");
    assert!(out.contains('●'), "{out}");
    assert!(out.contains("done"), "{out}");
}

#[test]
fn state_dangling_chain_falls_back() {
    let out = plain("stateDiagram-v2\n A --> B -->");
    assert!(out.contains("mermaid: stateDiagram-v2"), "{out}");
}

#[test]
fn sequence_renders_actors_and_messages() {
    let out = plain("sequenceDiagram\n Alice->>Bob: Hello Bob\n Bob-->>Alice: Hi Alice");
    assert!(out.contains("Alice"), "{out}");
    assert!(out.contains("Bob"), "{out}");
    assert!(out.contains("Hello Bob"), "{out}");
    assert!(out.contains('▶'), "solid call arrow:\n{out}");
    assert!(out.contains('◄'), "reply arrow:\n{out}");
    assert!(out.contains('╌'), "reply line is dashed:\n{out}");
    assert_eq!(
        out.matches("│ Alice │").count(),
        2,
        "actor boxes repeat at bottom:\n{out}"
    );
}

#[test]
fn sequence_participant_as_label() {
    let out =
        plain("sequenceDiagram\n participant C as Client\n participant S as Server\n C->>S: GET /");
    assert!(out.contains("Client"), "{out}");
    assert!(out.contains("Server"), "{out}");
}

#[test]
fn sequence_declared_order_wins() {
    let out = plain("sequenceDiagram\n participant B\n participant A\n A->>B: hi");
    let line = out.lines().nth(1).unwrap();
    assert!(
        line.find('B') < line.find('A'),
        "B declared first sits left:\n{out}"
    );
}

#[test]
fn sequence_self_message_loops() {
    let out = plain("sequenceDiagram\n A->>A: think");
    assert!(out.contains('╮'), "{out}");
    assert!(out.contains('╯'), "{out}");
    assert!(out.contains("think"), "{out}");
}

#[test]
fn sequence_cross_head() {
    let out = plain("sequenceDiagram\n A-x B: lost");
    assert!(out.contains('×'), "{out}");
}

#[test]
fn sequence_note_over_renders_box() {
    let out = plain("sequenceDiagram\n A->>B: hi\n Note over A,B: happy path");
    assert!(out.contains("happy path"), "{out}");
}

#[test]
fn sequence_autonumber_prefixes_messages() {
    let out = plain("sequenceDiagram\n autonumber\n A->>B: one\n B->>A: two");
    assert!(out.contains("1. one"), "{out}");
    assert!(out.contains("2. two"), "{out}");
}

#[test]
fn sequence_loop_renders_divider_and_end() {
    let out = plain("sequenceDiagram\n A->>B: hi\n loop retry x3\n A->>B: again\n end");
    assert!(out.contains("loop retry x3"), "{out}");
    assert!(out.contains(" end "), "{out}");
}

#[test]
fn sequence_rect_block_is_invisible() {
    let out = plain("sequenceDiagram\n rect rgb(0,0,0)\n A->>B: hi\n end");
    assert!(!out.contains("rect"), "{out}");
    assert!(!out.contains(" end "), "rect end is silent:\n{out}");
}

#[test]
fn sequence_box_end_does_not_close_enclosing_block() {
    let out = plain(
        "sequenceDiagram\n loop l1\n box g\n participant A\n end\n A->>B: hi\n A->>B: bye\n end",
    );
    assert_eq!(
        out.matches(" end ").count(),
        1,
        "box end is silent, loop end renders:\n{out}"
    );
    let lines: Vec<&str> = out.lines().collect();
    let row = |needle: &str| lines.iter().position(|l| l.contains(needle)).unwrap();
    assert!(
        row("loop l1") < row("hi") && row("bye") < row(" end "),
        "messages stay inside the loop:\n{out}"
    );
    assert!(!out.contains("box"), "{out}");
}

#[test]
fn sequence_critical_option_renders_dividers() {
    let out = plain(
        "sequenceDiagram\n critical connect\n A->>B: try\n option timeout\n A->>A: log\n end",
    );
    assert!(
        out.contains("critical connect"),
        "valid critical diagram renders:\n{out}"
    );
    assert!(out.contains("option timeout"), "{out}");
    assert!(out.contains(" end "), "{out}");
}

#[test]
fn sequence_long_label_widens_gap() {
    let out =
        plain("sequenceDiagram\n A->>B: a very long message label that needs room\n B-->>A: ok");
    assert!(
        out.contains("a very long message label that needs room"),
        "{out}"
    );
}

#[test]
fn mixed_solid_and_dotted_bus_stays_light() {
    let out = plain("graph TD\n A --> C\n B -.-> C");
    assert!(out.contains('╌'), "dotted branch survives:\n{out}");
    assert!(out.contains('─'), "solid branch survives:\n{out}");
    assert!(out.contains('┬'), "shared merge cell stays light:\n{out}");
}

#[test]
fn box_borders_stay_light_next_to_styled_edges() {
    let out = plain("graph TD\n A ==> B");
    assert!(out.contains('┌') && out.contains('└'), "{out}");
    assert!(!out.contains('┏'), "borders not restyled:\n{out}");
}

#[test]
fn self_loop_renders_below_box() {
    let out = plain("graph TD\n A --> A");
    assert!(out.contains('╰') && out.contains('╯'), "{out}");
    assert!(out.contains('▲'), "loop returns into the box:\n{out}");
}

#[test]
fn self_loop_label_renders() {
    let out = plain("graph TD\n A -->|again| A");
    assert!(out.contains("again"), "{out}");
}

#[test]
fn self_loop_coexists_with_forward_edge() {
    let out = plain("graph TD\n A --> A\n A --> B");
    assert!(out.contains('▲'), "{out}");
    assert!(out.contains('▼'), "{out}");
    assert!(out.contains('B'), "{out}");
    assert!(!out.contains('┼'), "{out}");
}

#[test]
fn self_loop_flips_with_bt() {
    let out = plain("flowchart BT\n A --> A\n A --> B");
    assert!(out.contains('▼'), "flipped loop head points down:\n{out}");
    assert!(out.contains('╭') || out.contains('╮'), "{out}");
}

#[test]
fn self_loop_in_lr() {
    let out = plain("flowchart LR\n A --> A\n A --> B");
    assert!(out.contains('▲'), "{out}");
    assert!(out.contains('▶'), "{out}");
}

#[test]
fn inline_o_word_label_still_parses_as_label() {
    let g = parse_graph("graph TD\n A -- or else --> B").unwrap();
    assert_eq!(g.nodes.len(), 2);
    assert_eq!(edge(&g, 0).label.as_deref(), Some("or else"));
}

#[test]
fn sequence_unparseable_arrow_falls_back() {
    let out = plain("sequenceDiagram\n ->>B: orphan");
    assert!(out.contains("mermaid: sequenceDiagram"), "{out}");
}

#[test]
fn sequence_unknown_statement_falls_back() {
    let out = plain("sequenceDiagram\n A->>B: hi\n garbage statement here");
    assert!(out.contains("mermaid: sequenceDiagram"), "{out}");
}

#[test]
fn sequence_over_wide_falls_back() {
    let out = plain_lines(
        &render(
            "sequenceDiagram\n A->>B: this label is far wider than the available pane width",
            &styles(),
            Some(30),
        )
        .unwrap(),
    )
    .join("\n");
    assert!(out.contains("mermaid: sequenceDiagram"), "{out}");
}

#[test]
fn sequence_over_cap_falls_back() {
    let mut src = String::from("sequenceDiagram\n");
    for i in 0..600 {
        src.push_str(" A->>B: msg ");
        src.push_str(&i.to_string());
        src.push('\n');
    }
    let out = plain(&src);
    assert!(out.contains("mermaid: sequenceDiagram"), "{out}");
}

#[test]
fn sequence_activation_markers_are_stripped() {
    let out = plain("sequenceDiagram\n A->>+B: call\n B-->>-A: return");
    assert!(out.contains("call"), "{out}");
    assert!(out.contains("return"), "{out}");
    assert!(!out.contains('+'), "{out}");
}

#[test]
fn sequence_rows_are_rectangular_and_sentinel_free() {
    let out = plain("sequenceDiagram\n Alice->>Bob: hi\n Note over Alice: solo note");
    assert!(!out.contains(CONT), "sentinel leaked:\n{out}");
    assert!(out.contains("solo note"), "{out}");
}

#[test]
fn hyphenated_flowchart_ids_stay_whole() {
    let g = parse_graph(
        "flowchart TD\n  pa-cli --> pa-types\n  pa-cli --> pa-core\n  pa-core --> pa-types",
    )
    .unwrap();
    assert_eq!(
        edge_ids(&g),
        vec![
            ("pa-cli".to_string(), "pa-core".to_string(), None),
            ("pa-cli".to_string(), "pa-types".to_string(), None),
            ("pa-core".to_string(), "pa-types".to_string(), None),
        ]
    );
}

#[test]
fn hyphenated_decision_ids_keep_their_labels() {
    let src = r#"flowchart TD
    start-node["prompt submitted"] --> check-lock{"session lease held by another client?"}
    check-lock -- yes --> reject-path["reply session_already_active"]
    check-lock -- no --> check-model{"model allowed?"}
    check-model -- no --> deny-model["reply allowlist error"]
    check-model -- yes --> run-turn["stream turn"]
    run-turn --> build-ok{"tool call in reply?"}
    build-ok -- yes --> exec-tool{"tool confirmed safe?"}
    exec-tool -- no --> blocked-error["emit error tool result"]
    exec-tool -- yes --> run-tool["execute tool"]
    run-tool --> run-turn
    build-ok -- no --> ctx-check{"context over budget?"}
    ctx-check -- yes --> do-compact["compact, then continue"]
    do-compact --> run-turn
    ctx-check -- no --> finish-ok["persist message, idle"]
"#;
    let g = parse_graph(src).unwrap();
    let mut ids: Vec<&str> = g.index.keys().map(String::as_str).collect();
    ids.sort_unstable();
    assert_eq!(
        ids,
        [
            "blocked-error",
            "build-ok",
            "check-lock",
            "check-model",
            "ctx-check",
            "deny-model",
            "do-compact",
            "exec-tool",
            "finish-ok",
            "reject-path",
            "run-tool",
            "run-turn",
            "start-node",
        ]
    );
    let lock = &g.nodes[index_of(&g, "check-lock")];
    assert_eq!(lock.label, "session lease held by another client?");
    assert!(
        lock.shape == Shape::Diamond,
        "shape of check-lock is not a diamond"
    );
}

#[test]
fn quoted_edge_label_keeps_its_hyphen() {
    let g = parse_graph("flowchart LR\n  CLI -- \"spawn/attach/control-plane commands\" --> SUP")
        .unwrap();
    assert_eq!(
        edge_ids(&g),
        vec![(
            "CLI".to_string(),
            "SUP".to_string(),
            Some("spawn/attach/control-plane commands".to_string()),
        )]
    );
}

#[test]
fn sequence_note_keeps_its_semicolon() {
    let art = render(
        "sequenceDiagram\n  A->>B: hi\n  Note over A,B: first; second",
        &styles(),
        Some(120),
    )
    .unwrap();
    let lines = plain_lines(&art);
    assert!(
        !lines.iter().any(|l| l.contains("mermaid: sequenceDiagram")),
        "fell back to the framed source:\n{}",
        lines.join("\n")
    );
    assert!(
        lines.iter().any(|l| l.contains("first; second")),
        "{}",
        lines.join("\n")
    );
}

#[test]
fn flowchart_semicolons_still_split_statements() {
    let g = parse_graph("flowchart LR\n  A-->B; B-->C").unwrap();
    assert_eq!(
        edge_ids(&g),
        vec![
            ("A".to_string(), "B".to_string(), None),
            ("B".to_string(), "C".to_string(), None),
        ]
    );
}

#[test]
fn bt_multiline_label_keeps_line_order() {
    let art = render(
        "flowchart BT\n  a[\"alpha beta gamma delta epsilon zeta eta\"] --> b[target]",
        &styles(),
        Some(120),
    )
    .unwrap();
    let lines = plain_lines(&art);
    let row_of = |word: &str| {
        lines
            .iter()
            .position(|l| l.contains(word))
            .unwrap_or_else(|| panic!("no row contains {word:?}:\n{}", lines.join("\n")))
    };
    assert!(row_of("alpha") < row_of("epsilon"), "{}", lines.join("\n"));
}

const STATE_DEEPSEEK: &str = r"stateDiagram-v2
    [*] --> idle
    idle --> streaming : prompt admitted
    streaming --> idle : final answer (TurnEnd)
    streaming --> toolRunning : ToolCall emitted
    state toolRunning {
        [*] --> executing
        executing --> stalled : heartbeat with no progress
        stalled --> executing : ToolExecutionProgress
        executing --> [*] : ToolExecutionEnd
    }
    toolRunning --> streaming : tool result sent to provider
    streaming --> compacting : context over budget
    toolRunning --> compacting : context over budget
    compacting --> idle : summary written
    compacting --> aborted : user cancels
    streaming --> aborted : user interrupts
    aborted --> idle : cleanup
    idle --> ended : delete or daemon shutdown
    ended --> [*]
";

const STATE_INKLING: &str = r"stateDiagram-v2
    [*] --> idle
    idle --> compacting: context full
    compacting --> idle: compacted
    idle --> session-active: user prompt
    state session-active {
        [*] --> streaming
        streaming --> tool-running: tool call
        tool-running --> streaming: tool result
    }
    session-active --> compacting: context full
    session-active --> ended: agent end / abort
    idle --> ended: cancel
    ended --> [*]
";

#[test]
fn composite_state_becomes_a_group() {
    let graph = parse_state(STATE_DEEPSEEK).unwrap();
    assert_eq!(
        graph.groups.len(),
        1,
        "{:?}",
        graph.groups.iter().map(|g| &g.id).collect::<Vec<_>>()
    );
    let group = graph
        .groups
        .iter()
        .position(|g| g.id == "toolRunning")
        .unwrap();
    let node_in = |label: &str| {
        let i = graph
            .nodes
            .iter()
            .position(|n| n.label == label)
            .unwrap_or_else(|| panic!("missing node {label:?}"));
        graph.node_group.get(i).copied().flatten()
    };
    assert_eq!(node_in("executing"), Some(group));
    assert_eq!(node_in("stalled"), Some(group));
    let dots: Vec<Option<usize>> = graph
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.label == "●")
        .map(|(i, _)| graph.node_group.get(i).copied().flatten())
        .collect();
    assert_eq!(dots.len(), 4, "{dots:?}");
    assert_eq!(
        dots.iter().filter(|g| **g == Some(group)).count(),
        2,
        "{dots:?}"
    );
}

#[test]
fn composite_state_draws_a_frame() {
    let art = render(STATE_DEEPSEEK, &styles(), Some(140)).unwrap();
    let lines = plain_lines(&art);
    // The composite state is a titled frame (`┌ toolRunning ───┐`), not a plain box.
    assert!(
        lines.iter().any(|l| l.contains("┌ toolRunning ─")),
        "{}",
        lines.join("\n")
    );
}

/// Every connector tag name appears exactly twice in the whole diagram (beside its source and
/// its target), even when edges sit in different composite-state frames.
#[test]
fn tag_names_are_unique_across_frames() {
    let text = plain_lines(&render(STATE_INKLING, &styles(), Some(140)).unwrap()).join("\n");
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for (start, _) in text.match_indices('(') {
        let name: String = text[start + 1..]
            .chars()
            .take_while(char::is_ascii_lowercase)
            .collect();
        let closes = text[start + 1 + name.len()..].starts_with([')', ':']);
        if !name.is_empty() && closes {
            *counts.entry(name).or_default() += 1;
        }
    }
    assert_eq!(
        counts,
        [("a", 2), ("b", 2), ("c", 2), ("d", 2)]
            .into_iter()
            .map(|(name, n)| (name.to_string(), n))
            .collect(),
        "{text}"
    );
}

#[test]
fn state_labels_are_drawn_in_full_deepseek() {
    let art = render(STATE_DEEPSEEK, &styles(), Some(140)).unwrap();
    let text = plain_lines(&art).join("\n");
    let expected = [
        "prompt admitted",
        "final answer (TurnEnd)",
        "ToolCall emitted",
        "heartbeat with no progress",
        "ToolExecutionProgress",
        "ToolExecutionEnd",
        "tool result sent to provider",
        "context over budget",
        "summary written",
        "user cancels",
        "user interrupts",
        "cleanup",
        "delete or daemon shutdown",
    ];
    let missing: Vec<&str> = expected
        .into_iter()
        .filter(|label| !text.contains(label))
        .collect();
    assert_eq!(missing, Vec::<&str>::new(), "{text}");
}

#[test]
fn state_labels_are_drawn_in_full_inkling() {
    let art = render(STATE_INKLING, &styles(), Some(140)).unwrap();
    let text = plain_lines(&art).join("\n");
    let expected = [
        "context full",
        "compacted",
        "user prompt",
        "tool call",
        "tool result",
        "agent end / abort",
        "cancel",
    ];
    let missing: Vec<&str> = expected
        .into_iter()
        .filter(|label| !text.contains(label))
        .collect();
    assert_eq!(missing, Vec::<&str>::new(), "{text}");
}
