use std::time::Duration;

use merman::svg::{
    DeterministicTextMeasurer, HostTheme, HostThemeAppearance, MeasurementProfileId, Presentation,
    SvgPipeline, TextMeasurementPolicy, TextMeasurementProfile, TextMeasurementProfileIdentity,
    TextStyle, ThemeRole,
};
use merman::{
    Engine, MermaidConfig, OperationControl, RenderError, RenderOutput, RenderRequest, Renderer,
    SvgEnvironment, SvgRequest,
};

use crate::{Style, Svg, finish, fonts::FONTS, hex, mix};

// a diagram that has not been laid out by now is not going to be readable.
const DEADLINE: Duration = Duration::from_secs(5);

pub(crate) fn render(source: &str, style: &Style) -> Result<Svg, String> {
    if source.trim().is_empty() {
        return Err("the diagram is empty".into());
    }

    let resolved = Presentation::new()
        .with_theme(theme(style).map_err(|error| error.to_string())?)
        .resolve();
    let engine = resolved
        .materialize_engine(Engine::new())
        .with_site_config(site_config());
    let request = SvgRequest {
        environment: SvgEnvironment::deterministic().with_text_measurement_policy(measurement()?),
        pipeline: Some(SvgPipeline::resvg_safe()),
        presentation: resolved.render_policy(),
        ..Default::default()
    };
    let control = OperationControl::new().with_deadline(DEADLINE);

    let output = Renderer::new()
        .with_engine(engine)
        .render(RenderRequest::svg(source, control, request))
        .map_err(|error| describe(&error, source))?;
    match output {
        RenderOutput::Svg(Some(svg)) => finish(svg.svg()),
        _ => Err(describe(&RenderError::NoDiagram, source)),
    }
}

fn site_config() -> MermaidConfig {
    MermaidConfig::from_value(serde_json::json!({
        // html labels are emitted as <foreignObject>, which the rasteriser
        // cannot draw. the library can rewrite them to svg text afterwards,
        // but that pass alone takes ~130 ms on a 200-node flowchart; asking
        // for svg text labels up front skips it.
        "htmlLabels": false,
        "flowchart": { "htmlLabels": false },
        // the default look adds drop-shadow filters, which are slow to
        // rasterise and muddy on dark backgrounds.
        "look": "classic",
    }))
}

fn theme(style: &Style) -> Result<HostTheme, merman::svg::PresentationError> {
    let Style {
        text,
        muted,
        background,
        accent,
        ..
    } = *style;
    let appearance = if style.dark {
        HostThemeAppearance::Dark
    } else {
        HostThemeAppearance::Light
    };
    // everything is derived from the four colours we are given, so a diagram
    // sits in the transcript like the text around it instead of bringing its
    // own palette.
    let border = mix(background, accent, 0.7);
    let roles = [
        (ThemeRole::Canvas, background),
        (ThemeRole::Surface, mix(background, accent, 0.12)),
        (ThemeRole::SurfaceAlt, mix(background, text, 0.05)),
        (ThemeRole::SurfaceMuted, mix(background, text, 0.1)),
        (ThemeRole::Text, text),
        (ThemeRole::SubtleText, muted),
        (ThemeRole::Border, border),
        (ThemeRole::Line, muted),
        (ThemeRole::EdgeLabelBackground, background),
        (ThemeRole::ClusterBackground, mix(background, text, 0.04)),
        (ThemeRole::ClusterBorder, mix(background, text, 0.25)),
        (ThemeRole::NoteBackground, mix(background, accent, 0.22)),
        (ThemeRole::NoteBorder, border),
        (ThemeRole::NoteText, text),
        (ThemeRole::ActorBackground, mix(background, accent, 0.12)),
        (ThemeRole::ActorBorder, border),
        (ThemeRole::ActorText, text),
        (ThemeRole::ActivationBackground, mix(background, accent, 0.3)),
        (ThemeRole::ActivationBorder, border),
    ];

    let mut theme = HostTheme::new()
        .with_appearance(appearance)
        .try_with_font_family(format!("\"{}\"", FONTS.family()))?
        .try_with_font_size(format!("{}px", style.font_px.clamp(6.0, 64.0)))?
        .try_with_series_palette(series(style).map(hex))?;
    for (role, colour) in roles {
        theme = theme.try_with_role(role, hex(colour))?;
    }
    Ok(theme)
}

/// colours for pie slices, gantt sections and the like: the accent first,
/// then hues that stay apart from each other on either background.
fn series(style: &Style) -> [[u8; 3]; 6] {
    let rest: [[u8; 3]; 5] = if style.dark {
        [
            [0x4f, 0xb3, 0x8a],
            [0xd9, 0x9a, 0x4e],
            [0xc0, 0x7a, 0xd6],
            [0xd8, 0x6b, 0x78],
            [0x5a, 0xb0, 0xc8],
        ]
    } else {
        [
            [0x2e, 0x8b, 0x67],
            [0xc0, 0x7a, 0x1e],
            [0x96, 0x4f, 0xb8],
            [0xc2, 0x45, 0x56],
            [0x2b, 0x88, 0xa6],
        ]
    };
    [style.accent, rest[0], rest[1], rest[2], rest[3], rest[4]]
}

fn measurement() -> Result<TextMeasurementPolicy, String> {
    let measurer = DeterministicTextMeasurer::default().with_width_callback(
        |text: &str, style: &TextStyle| {
            let bold = style.font_weight.as_deref().is_some_and(is_bold);
            let italic = style
                .font_style
                .as_deref()
                .is_some_and(|value| matches!(value.trim(), "italic" | "oblique"));
            FONTS.width(
                text,
                style.font_family.as_deref(),
                style.font_size,
                bold,
                italic,
            )
        },
    );
    let identity = MeasurementProfileId::new("tessel.fontdb")
        .and_then(|id| TextMeasurementProfileIdentity::new(id, env!("CARGO_PKG_VERSION")))
        .map_err(|error| error.to_string())?;
    Ok(TextMeasurementPolicy::uniform(TextMeasurementProfile::new(
        identity, measurer,
    )))
}

fn is_bold(weight: &str) -> bool {
    let weight = weight.trim();
    matches!(weight, "bold" | "bolder") || weight.parse::<f32>().is_ok_and(|n| n >= 600.0)
}

/// a one-line message that says where in the source the problem is.
fn describe(error: &RenderError, source: &str) -> String {
    match error {
        RenderError::NoDiagram => unknown_type(source),
        RenderError::Parse(diagnostic) => {
            let message = diagnostic.terminal_safe_message();
            if message.starts_with("No Mermaid diagram type") {
                return unknown_type(source);
            }
            // "Diagram parse error (flowchart-v2): ..." says nothing the line number does not.
            let message = message
                .split_once("): ")
                .filter(|(head, _)| head.starts_with("Diagram parse error"))
                .map_or(message.as_str(), |(_, rest)| rest);
            match diagnostic.terminal_diagnostic_details().span {
                Some(span) => format!("line {}: {message}", line_of(source, span.start)),
                None => message.to_owned(),
            }
        }
        RenderError::Cancelled(_) => {
            format!("the diagram took longer than {} s to lay out", DEADLINE.as_secs())
        }
        other => other.to_string(),
    }
}

fn unknown_type(source: &str) -> String {
    let (line, first) = source
        .lines()
        .enumerate()
        .find(|(_, line)| !line.trim().is_empty())
        .unwrap_or_default();
    let shown: String = first.trim().chars().take(40).collect();
    format!(
        "line {}: `{shown}` is not a diagram type (expected `flowchart`, `sequenceDiagram`, `classDiagram`, ...)",
        line + 1
    )
}

fn line_of(source: &str, byte: usize) -> usize {
    let end = byte.min(source.len());
    1 + source.as_bytes()[..end].iter().filter(|b| **b == b'\n').count()
}
