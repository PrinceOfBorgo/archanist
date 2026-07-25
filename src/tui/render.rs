use crate::config::ArchanistConfig;
use crate::state::ArchanistState;
use crate::tui::app::App;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};

pub fn draw(frame: &mut Frame, app: &App, cfg: &ArchanistConfig, state: &ArchanistState) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(frame.area());
    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(30), Constraint::Min(0)])
        .split(outer[0]);

    draw_components(frame, panes[0], app);
    draw_details(frame, panes[1], app, cfg, state);
    draw_footer(frame, outer[1]);
}

fn draw_components(frame: &mut Frame, area: Rect, app: &App) {
    let items: Vec<ListItem> = app
        .component_names
        .iter()
        .map(|n| ListItem::new(n.as_str()))
        .collect();
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("Components"))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▶ ");
    let mut ls = ListState::default();
    if !app.component_names.is_empty() {
        ls.select(Some(app.selected));
    }
    frame.render_stateful_widget(list, area, &mut ls);
}

fn draw_details(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    cfg: &ArchanistConfig,
    state: &ArchanistState,
) {
    let block = Block::default().borders(Borders::ALL).title("Details");
    let mut lines: Vec<Line> = Vec::new();

    let Some(name) = app.selected_component() else {
        lines.push(Line::from("(no components configured)"));
        frame.render_widget(Paragraph::new(lines).block(block), area);
        return;
    };

    let comp = &cfg.components[name];
    lines.push(Line::from(vec![Span::styled(
        format!("=== {} ===", name),
        Style::default().add_modifier(Modifier::BOLD),
    )]));
    if let Some(desc) = &comp.description {
        lines.push(Line::from(format!("description: {desc}")));
    }

    if let Some(s) = state.components.get(name) {
        lines.push(Line::from(""));
        lines.push(Line::from(format!(
            "current:  {}",
            s.current_version.as_deref().unwrap_or("unknown")
        )));
        lines.push(Line::from(format!(
            "previous: {}",
            s.previous_version.as_deref().unwrap_or("none")
        )));
        if let Some(latest) = &s.latest_check_version {
            lines.push(Line::from(format!("latest:   {latest}")));
        }
        if !s.blocklist.is_empty() {
            lines.push(Line::from(format!("blocked:  {}", s.blocklist.join(", "))));
        }
    } else {
        lines.push(Line::from(""));
        lines.push(Line::from("(no state yet)"));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(format!("Steps ({}):", comp.steps.len())));
    for (i, step) in comp.steps.iter().enumerate() {
        lines.push(Line::from(format!(
            "  {}. [{}] {}",
            i + 1,
            step.kind,
            step.id
        )));
    }

    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_footer(frame: &mut Frame, area: Rect) {
    let p = Paragraph::new("  [j/k] navigate    [q/esc] quit")
        .style(Style::default().add_modifier(Modifier::DIM));
    frame.render_widget(p, area);
}
