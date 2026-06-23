use crate::{component, element, AnyElement, Color, FlexDirection, Props, Weight};

use super::{Fragment, Text, View};

/// Source location attached to an [`ErrorOverview`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ErrorLocation {
    /// Source file path.
    pub file: String,
    /// 1-based source line.
    pub line: usize,
    /// 1-based source column, when known.
    pub column: Option<usize>,
}

/// Props for [`ErrorOverview`].
#[non_exhaustive]
#[derive(Default, Props)]
pub struct ErrorOverviewProps {
    /// Human-readable error message.
    pub message: String,
    /// Optional source location for the first application stack frame.
    pub location: Option<ErrorLocation>,
    /// Optional stack/backtrace text. Lines are rendered below the excerpt.
    pub stack: Option<String>,
    /// Optional source text used for the excerpt. If omitted and
    /// [`Self::location`] has a readable file path, the component reads that
    /// file synchronously, mirroring CC Ink's error overlay fallback path.
    pub source: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ErrorExcerptLine {
    line: usize,
    value: String,
}

fn cleanup_path(path: &str) -> String {
    let path = path.strip_prefix("file://").unwrap_or(path);
    if let Ok(cwd) = std::env::current_dir() {
        if let Ok(relative) = std::path::Path::new(path).strip_prefix(cwd) {
            return relative.display().to_string();
        }
    }
    path.to_string()
}

fn format_location(location: &ErrorLocation) -> String {
    let file = cleanup_path(&location.file);
    match location.column {
        Some(column) => format!("{}:{}:{}", file, location.line, column),
        None => format!("{}:{}", file, location.line),
    }
}

fn source_for_excerpt(props: &ErrorOverviewProps) -> Option<String> {
    if let Some(source) = &props.source {
        return Some(source.clone());
    }
    let location = props.location.as_ref()?;
    std::fs::read_to_string(cleanup_path(&location.file)).ok()
}

fn build_excerpt(props: &ErrorOverviewProps) -> Vec<ErrorExcerptLine> {
    let Some(location) = props.location.as_ref() else {
        return Vec::new();
    };
    if location.line == 0 {
        return Vec::new();
    }
    let Some(source) = source_for_excerpt(props) else {
        return Vec::new();
    };

    let radius = 2usize;
    let start = location.line.saturating_sub(radius).max(1);
    let end = location.line.saturating_add(radius);
    source
        .lines()
        .enumerate()
        .filter_map(|(index, value)| {
            let line = index + 1;
            (line >= start && line <= end).then(|| ErrorExcerptLine {
                line,
                value: value.to_string(),
            })
        })
        .collect()
}

fn stack_rows(stack: &str) -> Vec<AnyElement<'static>> {
    stack
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            element! {
                View {
                    Text(content: "- ", weight: Weight::Light)
                    Text(content: line.to_string(), weight: Weight::Bold)
                }
            }
            .into_any()
        })
        .collect()
}

/// Renders a CC Ink-style error overlay.
///
/// The component mirrors `src/ink/components/ErrorOverview.tsx`: it shows a red
/// `ERROR` badge, the error message, an optional source location, a small source
/// excerpt with the failing line highlighted, and optional stack/backtrace rows.
/// It is intentionally UI-only; callers decide where the message, location, and
/// stack data come from.
#[component]
pub fn ErrorOverview(props: &ErrorOverviewProps) -> impl Into<AnyElement<'static>> {
    let location_text = props.location.as_ref().map(format_location);
    let excerpt = build_excerpt(props);
    let line_width = excerpt
        .iter()
        .map(|line| line.line.to_string().len())
        .max()
        .unwrap_or(0);
    let current_line = props.location.as_ref().map(|location| location.line);

    let excerpt_rows: Vec<AnyElement<'static>> = excerpt
        .into_iter()
        .map(|line| {
            let is_current = current_line == Some(line.line);
            let background = is_current.then_some(Color::Red);
            let color = is_current.then_some(Color::White);
            let line_weight = if is_current {
                Weight::Bold
            } else {
                Weight::Light
            };
            element! {
                View {
                    View(width: (line_width + 1) as u32) {
                        Text(
                            content: format!("{:>width$}:", line.line, width = line_width),
                            color: color,
                            background_color: background,
                            weight: line_weight,
                        )
                    }
                    Text(
                        content: format!(" {}", line.value),
                        color: color,
                        background_color: background,
                    )
                }
            }
            .into_any()
        })
        .collect();

    let stack_rows = props.stack.as_deref().map(stack_rows).unwrap_or_default();

    element! {
        View(flex_direction: FlexDirection::Column, padding: 1) {
            View {
                Text(content: " ERROR ", color: Color::White, background_color: Color::Red)
                Text(content: format!(" {}", props.message))
            }
            #(location_text.map(|location| element! {
                View(margin_top: 1) {
                    Text(content: location, weight: Weight::Light)
                }
            }))
            #(if excerpt_rows.is_empty() {
                element!(Fragment).into_any()
            } else {
                element! {
                    View(margin_top: 1, flex_direction: FlexDirection::Column) {
                        #(excerpt_rows)
                    }
                }
                .into_any()
            })
            #(if stack_rows.is_empty() {
                element!(Fragment).into_any()
            } else {
                element! {
                    View(margin_top: 1, flex_direction: FlexDirection::Column) {
                        #(stack_rows)
                    }
                }
                .into_any()
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::prelude::*;

    #[test]
    fn test_error_overview_renders_badge_location_excerpt_and_stack() {
        let canvas = element!(ErrorOverview(
            message: "boom".to_string(),
            location: Some(ErrorLocation {
                file: "src/main.rs".to_string(),
                line: 3,
                column: Some(9),
            }),
            source: Some("fn main() {\n    ok();\n    boom();\n    after();\n}\n".to_string()),
            stack: Some("panic at boom\ncalled from test".to_string()),
        ))
        .render(None);

        let output = canvas.to_string();
        assert!(output.contains("ERROR  boom"), "badge/message: {output:?}");
        assert!(output.contains("src/main.rs:3:9"), "location: {output:?}");
        assert!(output.contains("3:     boom();"), "excerpt: {output:?}");
        assert!(output.contains("- panic at boom"), "stack: {output:?}");

        let has_highlighted_current_line_number = (0..canvas.height()).any(|y| {
            (0..canvas.width()).any(|x| {
                let Some(cell) = canvas.cell(x, y) else {
                    return false;
                };
                cell.text() == Some("3")
                    && cell.background_color == Some(Color::Red)
                    && canvas.resolved_text_style(x, y).unwrap().color == Some(Color::White)
            })
        });
        assert!(
            has_highlighted_current_line_number,
            "current excerpt line should be highlighted"
        );
    }

    #[test]
    fn test_error_overview_omits_unavailable_optional_sections() {
        let output = element!(ErrorOverview(message: "only message".to_string())).to_string();
        assert!(output.contains("ERROR  only message"));
    }
}
