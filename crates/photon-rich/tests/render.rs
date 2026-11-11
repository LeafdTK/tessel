//! renders real inputs and rasterises the result the way the app does.
//!
//! set `PHOTON_RICH_PNG_DIR` to a directory to also get the rasterised
//! samples written there as png, for looking at.

use std::{path::PathBuf, sync::Arc, time::Instant};

use photon_rich::{Style, Svg, math, mermaid};
use resvg::tiny_skia::{Color, Pixmap, PixmapPaint, Transform};

const LIGHT: Style = Style {
    dark: false,
    text: [0x1f, 0x23, 0x28],
    muted: [0x6b, 0x72, 0x80],
    background: [0xff, 0xff, 0xff],
    accent: [0x3b, 0x82, 0xf6],
    font_px: 14.0,
};

const DARK: Style = Style {
    dark: true,
    text: [0xe6, 0xe8, 0xeb],
    muted: [0x9a, 0xa2, 0xad],
    background: [0x16, 0x18, 0x1c],
    accent: [0x7a, 0xa2, 0xf7],
    font_px: 14.0,
};

// gpui renders an svg image at twice its natural size and shows it at half.
const SMOOTH_SVG_SCALE_FACTOR: f32 = 2.0;

/// what gpui's `SvgRenderer::render_single_frame` does with `ImageFormat::Svg`
/// bytes, minus the final rgba to bgra swap. `fonts` is the database its text
/// would be resolved against.
fn rasterise(svg: &Svg, fonts: usvg::fontdb::Database) -> Pixmap {
    let options = usvg::Options {
        fontdb: Arc::new(fonts),
        ..Default::default()
    };
    let tree = usvg::Tree::from_data(svg.data.as_bytes(), &options).expect("usvg parses the svg");
    let size = tree.size();
    assert!(size.width() > 0.0 && size.height() > 0.0, "svg has no size");
    assert!(
        (size.width() - svg.width).abs() < 0.01 && (size.height() - svg.height).abs() < 0.01,
        "reported size {}x{} is not the svg's {}x{}",
        svg.width,
        svg.height,
        size.width(),
        size.height()
    );
    let width = size.width() * SMOOTH_SVG_SCALE_FACTOR;
    let height = size.height() * SMOOTH_SVG_SCALE_FACTOR;
    let mut pixmap = Pixmap::new(width as u32, height as u32).expect("pixmap");
    let transform = Transform::from_scale(width / size.width(), height / size.height());
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    pixmap
}

fn system_fonts() -> usvg::fontdb::Database {
    let mut fonts = usvg::fontdb::Database::new();
    fonts.load_system_fonts();
    fonts
}

fn no_fonts() -> usvg::fontdb::Database {
    usvg::fontdb::Database::new()
}

/// pixels that are mostly opaque and within `tolerance` of `colour`.
fn pixels_near(pixmap: &Pixmap, colour: [u8; 3], tolerance: i32) -> Vec<(u32, u32)> {
    let mut found = Vec::new();
    for y in 0..pixmap.height() {
        for x in 0..pixmap.width() {
            let pixel = pixmap.pixel(x, y).expect("in bounds").demultiply();
            let near = [pixel.red(), pixel.green(), pixel.blue()]
                .iter()
                .zip(colour)
                .all(|(have, want)| (i32::from(*have) - i32::from(want)).abs() <= tolerance);
            if pixel.alpha() > 200 && near {
                found.push((x, y));
            }
        }
    }
    found
}

fn inked(pixmap: &Pixmap) -> usize {
    pixmap.pixels().iter().filter(|p| p.alpha() > 0).count()
}

fn save(name: &str, svg: &Svg, style: &Style) {
    let Some(dir) = std::env::var_os("PHOTON_RICH_PNG_DIR").map(PathBuf::from) else {
        return;
    };
    std::fs::create_dir_all(&dir).expect("png dir");
    let drawing = rasterise(svg, no_fonts());
    // the drawing is transparent; put it on the background it is meant for.
    let margin = 16;
    let mut canvas =
        Pixmap::new(drawing.width() + 2 * margin, drawing.height() + 2 * margin).expect("canvas");
    let [r, g, b] = style.background;
    canvas.fill(Color::from_rgba8(r, g, b, 255));
    canvas.draw_pixmap(
        margin as i32,
        margin as i32,
        drawing.as_ref(),
        &PixmapPaint::default(),
        Transform::identity(),
        None,
    );
    canvas.save_png(dir.join(format!("{name}.png"))).expect("png written");
    std::fs::write(dir.join(format!("{name}.svg")), &svg.data).expect("svg written");
}

const FLOWCHART: &str = "flowchart TD
    A[User sends prompt] --> B{Needs a tool?}
    B -->|yes| C[Run tool call]
    B -->|no| D[Stream answer]
    C --> E[(Session store)]
    C --> F([Sandbox])
    F -.->|stdout| C
    E --> D
    subgraph daemon [photond]
        C
        E
        F
    end
    D ==> G((Done))
";

const SEQUENCE: &str = "sequenceDiagram
    autonumber
    participant U as User
    participant A as Agent
    participant T as Tool runner
    U->>A: fix the failing test
    activate A
    A->>T: cargo test -p photon-rich
    T-->>A: 1 failed, 12 passed
    Note over A,T: reads the failure, edits the file
    loop until green
        A->>T: cargo test
        T-->>A: result
    end
    alt still failing
        A-->>U: needs your help
    else fixed
        A-->>U: all 13 tests pass
    end
    deactivate A
";

const CLASS: &str = "classDiagram
    class Session {
        +Uuid id
        +String title
        -Vec~Message~ transcript
        +send(prompt) Result
        +cancel()
    }
    class Message {
        <<enumeration>>
        User
        Assistant
        ToolCall
    }
    class Agent {
        <<interface>>
        +run(session) Stream
    }
    Session \"1\" *-- \"many\" Message : contains
    Agent <|.. ClaudeAgent
    Agent <|.. CodexAgent
    Session --> Agent : drives
";

const STATE: &str = "stateDiagram-v2
    [*] --> Idle
    Idle --> Thinking : prompt
    Thinking --> ToolUse : tool call
    ToolUse --> Thinking : result
    Thinking --> Idle : done
    Thinking --> Failed : error
    state ToolUse {
        [*] --> Pending
        Pending --> Running : approved
        Running --> [*]
    }
    Failed --> [*]
";

const ER: &str = "erDiagram
    WORKSPACE ||--o{ SESSION : has
    SESSION ||--|{ MESSAGE : contains
    SESSION }o--|| AGENT : \"runs on\"
    WORKSPACE {
        uuid id PK
        string path
    }
    SESSION {
        uuid id PK
        uuid workspace_id FK
        string title
    }
    MESSAGE {
        uuid id PK
        string role
        text body
    }
";

const GANTT: &str = "gantt
    title Rich rendering rollout
    dateFormat YYYY-MM-DD
    section Library
    Pick crates        :done, a1, 2026-11-02, 3d
    photon-rich crate  :active, a2, after a1, 4d
    section App
    Transcript wiring  :b1, after a2, 5d
    Theme polish       :b2, after b1, 2d
    Ship               :milestone, after b2, 0d
";

const NESTED: &str = "flowchart LR
    subgraph Client
        direction TB
        UI[UI] --> State[State store]
    end
    subgraph Server
        direction TB
        subgraph API
            REST[REST] --> GQL[GraphQL]
        end
        subgraph Data
            DB[(Postgres)]
            Cache[(Redis)]
        end
        API --> Data
    end
    State <--> REST
    Client -. websocket .-> Server
";

/// every diagram type we claim, with a minimal valid source.
const DIAGRAMS: &[(&str, &str)] = &[
    ("flowchart", FLOWCHART),
    ("flowchart-nested", NESTED),
    ("sequence", SEQUENCE),
    ("class", CLASS),
    ("state", STATE),
    ("er", ER),
    ("gantt", GANTT),
    ("pie", "pie title Time spent\n    \"Reading\" : 45\n    \"Writing\" : 30\n    \"Waiting\" : 25\n"),
    (
        "mindmap",
        "mindmap\n  root((photon))\n    Daemon\n      Sessions\n      Agents\n    App\n      Transcript\n      Sidebar\n    CLI\n",
    ),
    (
        "journey",
        "journey\n    title Onboarding\n    section Sign up\n      Visit site: 5: User\n      Create account: 3: User\n    section First run\n      Open app: 4: User\n",
    ),
    (
        "gitgraph",
        "gitGraph\n    commit id: \"init\"\n    branch feature\n    checkout feature\n    commit\n    checkout main\n    commit tag: \"v1.0\"\n    merge feature\n",
    ),
    (
        "timeline",
        "timeline\n    title Project history\n    2024 : Prototype\n    2025 : Rewrite in Rust : First users\n    2026 : 1.0 release\n",
    ),
    (
        "quadrant",
        "quadrantChart\n    title Effort vs impact\n    x-axis Low effort --> High effort\n    y-axis Low impact --> High impact\n    quadrant-1 Plan\n    quadrant-2 Do now\n    quadrant-3 Skip\n    quadrant-4 Reconsider\n    Mermaid: [0.3, 0.8]\n    Video: [0.9, 0.2]\n",
    ),
    (
        "xychart",
        "xychart-beta\n    title \"Render time\"\n    x-axis [10, 50, 100, 200]\n    y-axis \"ms\" 0 --> 50\n    bar [1, 4, 9, 22]\n    line [1, 4, 9, 22]\n",
    ),
    (
        "requirement",
        "requirementDiagram\n    requirement fast_render {\n        id: 1\n        text: renders in 100ms\n        risk: medium\n        verifymethod: test\n    }\n    element bench {\n        type: test\n    }\n    bench - verifies -> fast_render\n",
    ),
    (
        "c4",
        "C4Context\n    title System Context\n    Person(user, \"User\", \"Uses the app\")\n    System(app, \"Photon\", \"Agent IDE\")\n    Rel(user, app, \"Uses\")\n",
    ),
    ("sankey", "sankey-beta\nInput,Parse,10\nParse,Layout,8\nParse,Error,2\nLayout,Render,8\n"),
    ("block", "block-beta\n    columns 3\n    a[\"Frontend\"] b[\"API\"] c[\"DB\"]\n    a --> b\n    b --> c\n"),
    (
        "architecture",
        "architecture-beta\n    group api(cloud)[API]\n    service db(database)[Database] in api\n    service server(server)[Server] in api\n    db:L -- R:server\n",
    ),
    ("kanban", "kanban\n  Todo\n    [Write docs]\n    id2[Add tests]\n  Done\n    id3[Ship]\n"),
];

#[test]
fn every_diagram_type_renders() {
    for (name, source) in DIAGRAMS {
        let svg = mermaid(source, &LIGHT).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert!(svg.width > 20.0 && svg.height > 20.0, "{name}: {}x{}", svg.width, svg.height);
        assert!(!svg.data.contains("<text"), "{name}: text was left for the rasteriser");
        assert!(!svg.data.contains("foreignObject"), "{name}: html label left in the svg");
        let pixmap = rasterise(&svg, no_fonts());
        assert!(inked(&pixmap) > 500, "{name}: nothing was drawn");
        save(&format!("type-{name}"), &svg, &LIGHT);
    }
}

#[test]
fn labels_are_drawn_without_any_fonts_at_the_rasteriser() {
    // no edges and no title: the only thing in the text colour is the label.
    let svg = mermaid("flowchart TD\n    A[Hello photon]\n", &LIGHT).expect("renders");
    let bare = rasterise(&svg, no_fonts());
    let label = pixels_near(&bare, LIGHT.text, 40);
    assert!(label.len() > 150, "only {} label pixels", label.len());

    // and they sit inside the node, in the middle of the image.
    let (width, height) = (bare.width(), bare.height());
    let (left, right) = (width / 8, width - width / 8);
    let (top, bottom) = (height / 8, height - height / 8);
    for (x, y) in &label {
        assert!((left..right).contains(x) && (top..bottom).contains(y), "label pixel at {x},{y} of {width}x{height}");
    }
    // the label spans most of the node: metrics used for layout match the glyphs drawn.
    let min = label.iter().map(|(x, _)| *x).min().expect("pixels");
    let max = label.iter().map(|(x, _)| *x).max().expect("pixels");
    let span = (max - min) as f32 / width as f32;
    assert!((0.35..0.9).contains(&span), "label spans {span} of the node");

    // a rasteriser that has every system font draws exactly the same thing.
    let with_fonts = rasterise(&svg, system_fonts());
    assert_eq!(bare.data(), with_fonts.data());
}

#[test]
fn sequence_and_class_text_is_drawn() {
    for (name, source) in [("sequence", SEQUENCE), ("class", CLASS), ("er", ER), ("state", STATE)] {
        let svg = mermaid(source, &DARK).expect(name);
        let pixmap = rasterise(&svg, no_fonts());
        let text = pixels_near(&pixmap, DARK.text, 40).len();
        assert!(text > 1500, "{name}: only {text} pixels in the text colour");
    }
}

#[test]
fn background_is_transparent() {
    for style in [&LIGHT, &DARK] {
        let svg = mermaid(FLOWCHART, style).expect("renders");
        let pixmap = rasterise(&svg, no_fonts());
        let (w, h) = (pixmap.width(), pixmap.height());
        for (x, y) in [(0, 0), (w - 1, 0), (0, h - 1), (w - 1, h - 1)] {
            assert_eq!(pixmap.pixel(x, y).expect("corner").alpha(), 0, "corner {x},{y} is painted");
        }
        let clear = pixmap.pixels().iter().filter(|p| p.alpha() == 0).count();
        assert!(clear * 4 > pixmap.pixels().len(), "less than a quarter of the image is transparent");
        let formula = math(r"\frac{a}{b}", true, style).expect("renders");
        let pixmap = rasterise(&formula, no_fonts());
        assert_eq!(pixmap.pixel(0, 0).expect("corner").alpha(), 0);
    }
}

#[test]
fn colours_follow_the_style() {
    for (name, source) in [("flowchart", FLOWCHART), ("sequence", SEQUENCE)] {
        let light = mermaid(source, &LIGHT).expect("light");
        let dark = mermaid(source, &DARK).expect("dark");
        assert_ne!(light.data, dark.data);
        // same layout, different paint.
        assert_eq!((light.width, light.height), (dark.width, dark.height));

        let light_pixels = rasterise(&light, no_fonts());
        let dark_pixels = rasterise(&dark, no_fonts());
        assert!(pixels_near(&light_pixels, LIGHT.text, 30).len() > 500, "{name}: light text colour missing");
        assert!(pixels_near(&dark_pixels, DARK.text, 30).len() > 500, "{name}: dark text colour missing");
        // nothing meant for the other mode may be baked into either rendering.
        assert_eq!(pixels_near(&dark_pixels, [255, 255, 255], 6).len(), 0, "{name}: white in dark mode");
        assert_eq!(pixels_near(&dark_pixels, [0, 0, 0], 6).len(), 0, "{name}: black in dark mode");
        assert_eq!(pixels_near(&light_pixels, DARK.background, 6).len(), 0, "{name}: dark fill in light mode");
        assert_eq!(pixels_near(&light_pixels, [0, 0, 0], 6).len(), 0, "{name}: black in light mode");

        save(&format!("{name}-light"), &light, &LIGHT);
        save(&format!("{name}-dark"), &dark, &DARK);
    }

    let light = math(r"e^{i\pi} + 1 = 0", false, &LIGHT).expect("light");
    let dark = math(r"e^{i\pi} + 1 = 0", false, &DARK).expect("dark");
    assert_ne!(light.data, dark.data);
    let dark_pixels = rasterise(&dark, no_fonts());
    assert!(pixels_near(&dark_pixels, DARK.text, 30).len() > 100);
    assert_eq!(pixels_near(&dark_pixels, [0x11, 0x11, 0x11], 12).len(), 0, "default ink left in dark mode");
}

#[test]
fn font_size_scales_the_drawing() {
    let big = Style { font_px: 21.0, ..LIGHT };
    let small = math(r"x^2 + y^2", false, &LIGHT).expect("small");
    let large = math(r"x^2 + y^2", false, &big).expect("large");
    assert!((large.width / small.width - 1.5).abs() < 0.02, "{} vs {}", large.width, small.width);

    // a flowchart's labels are set in the size asked for, so it grows. it grows
    // taller, not wider: the layout library gives every box a floor width that
    // short labels sit inside at any size, and wraps the label when it is large.
    let small = mermaid("flowchart LR\n    A[Some label] --> B[Another label]\n", &LIGHT).expect("small");
    let large = mermaid("flowchart LR\n    A[Some label] --> B[Another label]\n", &big).expect("large");
    assert!(large.height > small.height * 1.15, "{} vs {}", large.height, small.height);
    assert!(large.width >= small.width, "{} vs {}", large.width, small.width);
}

#[test]
fn invalid_mermaid_is_an_error_that_names_the_line() {
    let cases = [
        ("flowchart TD\n    A[Start] --> B\n    B --> \n    C -->| D\n", "line 4"),
        ("flowchart TD\n    A[unclosed --> B\n", "line 2"),
        ("sequenceDiagram\n    Alice->>Bob: hi\n    Bob-->>\n", "line 3"),
        ("classDiagram\n    class A {\n        +foo(\n    A <|--\n", "line"),
        ("notADiagram\n  foo --> bar\n", "line 1"),
    ];
    for (source, expected) in cases {
        let error = mermaid(source, &LIGHT).expect_err(source);
        println!("{error}");
        assert!(error.contains(expected), "`{error}` does not mention {expected} for:\n{source}");
        assert!(error.len() < 300 && !error.contains('\n'), "unwieldy message: {error}");
    }
    assert!(mermaid("", &LIGHT).is_err());
    assert!(mermaid("   \n\n", &LIGHT).is_err());
}

const FORMULAS: &[(&str, bool, &str)] = &[
    ("fraction", true, r"x = \frac{-b \pm \sqrt{b^2 - 4ac}}{2a}"),
    ("scripts", false, r"x_{i_j}^{k^2} + a_1^2 + e^{-x^2}"),
    ("sum", true, r"\sum_{n=1}^{\infty} \frac{1}{n^2} = \frac{\pi^2}{6}"),
    ("integral", true, r"\int_{-\infty}^{\infty} e^{-x^2}\,dx = \sqrt{\pi}"),
    ("greek", false, r"\alpha + \beta = \gamma \cdot \Delta\Omega \le \varepsilon"),
    (
        "matrix",
        true,
        r"\begin{pmatrix} a & b \\ c & d \end{pmatrix} \begin{bmatrix} x \\ y \end{bmatrix} = \mathbf{v}",
    ),
    ("text", true, r"P(A \mid B) = \frac{P(B \mid A)\,P(A)}{P(B)} \quad \text{if } P(B) > 0"),
    ("cases", true, r"f(x) = \begin{cases} x^2 & x \ge 0 \\ -x & \text{otherwise} \end{cases}"),
    (
        "aligned",
        true,
        r"\begin{aligned} \nabla \cdot \mathbf{E} &= \frac{\rho}{\varepsilon_0} \\ \nabla \times \mathbf{B} &= \mu_0 \mathbf{J} \end{aligned}",
    ),
    ("sets", false, r"\forall x \in \mathbb{R},\ \exists n \in \mathbb{N} : n > x"),
    ("operators", true, r"\operatorname{softmax}(z)_i = \frac{e^{z_i}}{\sum_j e^{z_j}}, \quad \lim_{x \to 0} \frac{\sin x}{x} = 1"),
    ("delimiters", true, r"\left\lfloor \frac{n}{2} \right\rfloor + \binom{n}{k} + \left\| x \right\|_2"),
];

#[test]
fn math_constructs_render() {
    for (name, display, tex) in FORMULAS {
        let svg = math(tex, *display, &LIGHT).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert!(svg.width > 10.0 && svg.height > 8.0, "{name}: {}x{}", svg.width, svg.height);
        assert!(svg.width < 900.0 && svg.height < 200.0, "{name}: {}x{}", svg.width, svg.height);
        assert!(!svg.data.contains("<text"), "{name}: text was left for the rasteriser");
        let pixmap = rasterise(&svg, no_fonts());
        let ink = pixels_near(&pixmap, LIGHT.text, 40).len();
        assert!(ink > 80, "{name}: only {ink} pixels of ink");
        save(&format!("math-{name}"), &svg, &LIGHT);
    }
}

#[test]
fn display_math_is_set_larger_than_inline() {
    let tex = r"\sum_{n=1}^{\infty} \frac{1}{n^2}";
    let inline = math(tex, false, &LIGHT).expect("inline");
    let display = math(tex, true, &LIGHT).expect("display");
    // limits go above and below the sum and the fraction is full size.
    assert!(display.height > inline.height * 1.3, "{} vs {}", display.height, inline.height);
    // an inline formula stays close to the height of a line of text.
    let simple = math("E = mc^2", false, &LIGHT).expect("inline");
    assert!(simple.height < LIGHT.font_px * 1.6, "inline formula is {} px tall", simple.height);

    for (name, style) in [("light", &LIGHT), ("dark", &DARK)] {
        save(&format!("math-inline-{name}"), &math(r"e^{i\pi} + 1 = 0", false, style).expect("inline"), style);
        let tex = r"\int_{-\infty}^{\infty} e^{-x^2}\,dx = \sqrt{\pi}";
        save(&format!("math-display-{name}"), &math(tex, true, style).expect("display"), style);
    }
}

#[test]
fn invalid_math_is_an_error_that_names_the_column() {
    let cases = [
        r"\frac{1}{",
        r"\unknowncommand{x}",
        r"a^b^c",
        r"\begin{pmatrix} a & b \\ c & d",
        r"\left( x + y",
    ];
    for tex in cases {
        let error = math(tex, true, &LIGHT).expect_err(tex);
        println!("{tex}  ->  {error}");
        assert!(error.contains("column"), "`{error}` does not say where, for {tex}");
        assert!(error.len() < 200 && !error.contains('\n'), "unwieldy message: {error}");
    }
    assert!(math("", false, &LIGHT).is_err());
    assert!(math("  ", true, &LIGHT).is_err());
}

#[test]
fn hostile_input_is_an_error_not_a_crash() {
    // each of these recurses as deep as its input is long in a naive parser.
    let deep_braces = format!("{}x{}", "{".repeat(100_000), "}".repeat(100_000));
    assert!(math(&deep_braces, true, &LIGHT).is_err());
    let deep_roots = format!("{}x", r"\sqrt ".repeat(2_500));
    let _ = math(&deep_roots, true, &LIGHT);
    let deep_fractions = format!("{}x{}", r"\frac{1}{".repeat(190), "}".repeat(190));
    let _ = math(&deep_fractions, true, &LIGHT);
    let deep_left = format!("{}x{}", r"\left(".repeat(5_000), r"\right)".repeat(5_000));
    assert!(math(&deep_left, true, &LIGHT).is_err());
    assert!(math(&"x+".repeat(100_000), true, &LIGHT).is_err());

    let mut nested = String::from("flowchart TD\n");
    for depth in 0..300 {
        nested.push_str(&format!("subgraph s{depth}\n"));
    }
    nested.push_str("A --> B\n");
    nested.push_str(&"end\n".repeat(300));
    let _ = mermaid(&nested, &LIGHT);

    let garbage = [
        "\u{0}\u{1}\u{2}",
        "flowchart TD\n\u{0}",
        "flowchart",
        "sequenceDiagram\n    ->>",
        "graph TD; A-->; -->B;;;",
        "pie\n    \"a\" : -1\n    \"b\" : NaN",
        "gantt\n    dateFormat YYYY-MM-DD\n    task :a, 9999-99-99, 1d",
        "%%{init: {\"theme\": 12, \"themeVariables\": []}}%%\nflowchart TD\n A-->B",
        "classDiagram\n class {{{{{{{{",
        "stateDiagram-v2\n [*] --> [*]\n state {",
        "erDiagram\n A ||--o{ : x",
        "\\\\\\\\\\",
        "$$$$",
        "\\begin{}",
        "\\end{pmatrix}",
        "}}}}{{{{",
        "\\frac",
        "^_^_^",
        "\\text{",
        "&&&\\\\&&",
    ];
    for input in garbage {
        let _ = mermaid(input, &LIGHT);
        let _ = math(input, true, &LIGHT);
        let _ = math(input, false, &DARK);
    }
}

fn big_flowchart(nodes: usize) -> String {
    let mut source = String::from("flowchart TD\n");
    for node in 0..nodes {
        source.push_str(&format!("    N{node}[Step {node}]\n"));
    }
    for node in 1..nodes {
        source.push_str(&format!("    N{} --> N{node}\n", (node - 1) / 2));
        if node % 7 == 0 {
            source.push_str(&format!("    N{node} -->|retry| N{}\n", node / 3));
        }
    }
    source
}

#[test]
fn two_hundred_nodes_render_in_a_tenth_of_a_second() {
    let source = big_flowchart(200);
    // the first call pays for loading the system font database once.
    let start = Instant::now();
    mermaid("flowchart TD\n    A --> B\n", &LIGHT).expect("warm-up renders");
    println!("first render (loads fonts): {:.1} ms", start.elapsed().as_secs_f64() * 1e3);

    let mut best = f64::MAX;
    for _ in 0..5 {
        let start = Instant::now();
        let svg = mermaid(&source, &LIGHT).expect("renders");
        best = best.min(start.elapsed().as_secs_f64() * 1e3);
        assert!(svg.width > 500.0);
    }
    println!("200-node flowchart: {best:.1} ms (best of 5)");

    let start = Instant::now();
    for (_, display, tex) in FORMULAS {
        math(tex, *display, &LIGHT).expect("renders");
    }
    let each = start.elapsed().as_secs_f64() * 1e3 / FORMULAS.len() as f64;
    println!("formula: {each:.2} ms each");

    assert!(best < 100.0, "200-node flowchart took {best:.1} ms");
    assert!(each < 10.0, "a formula took {each:.2} ms");
}
