#![allow(dead_code)] // TODO: decide what to do with it

use std::fmt;

use nu_ansi_term::{Color, Style};
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields};
use tracing_subscriber::registry::LookupSpan;

struct MessageExtractor {
    message: String,
    fields: Vec<(String, String)>,
}

impl MessageExtractor {
    fn new() -> Self {
        Self {
            message: String::new(),
            fields: Vec::new(),
        }
    }
}

impl Visit for MessageExtractor {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{:?}", value);
        } else {
            self.fields
                .push((field.name().to_string(), format!("{:?}", value)));
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else {
            self.fields
                .push((field.name().to_string(), value.to_string()));
        }
    }
}

pub struct CinemaFormatter;

impl<S, N> FormatEvent<S, N> for CinemaFormatter
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        _ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let ansi = writer.has_ansi_escapes();
        let level = *event.metadata().level();

        let level_color = match level {
            Level::ERROR => Color::Red,
            Level::WARN => Color::Yellow,
            Level::INFO => Color::Green,
            Level::DEBUG => Color::Blue,
            Level::TRACE => Color::Purple,
        };

        if ansi {
            write!(
                writer,
                "{} ",
                level_color.bold().paint(format!("{:>5}", level))
            )?;
        } else {
            write!(writer, "{:>5} ", level)?;
        }

        let mut visitor = MessageExtractor::new();
        event.record(&mut visitor);

        if ansi {
            write!(
                writer,
                "{}",
                Style::new().fg(level_color).paint(&visitor.message)
            )?;
            if !visitor.fields.is_empty() {
                let dimmed = Style::new().dimmed();
                for (key, val) in &visitor.fields {
                    write!(writer, " {}", dimmed.paint(format!("{}={}", key, val)))?;
                }
            }
        } else {
            write!(writer, "{}", visitor.message)?;
            for (key, val) in &visitor.fields {
                write!(writer, " {}={}", key, val)?;
            }
        }
        writeln!(writer)
    }
}
