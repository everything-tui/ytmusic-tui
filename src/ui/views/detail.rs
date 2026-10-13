use crate::action::Action;
use crate::api::model::{Album, Artist, Playlist, Track};
use crate::ui::components::{pill, truncate};
use crate::ui::cover::{CoverCtx, CoverSlot};
use crate::ui::theme::Theme;
use crate::ui::views::library::artist_line;
use crate::ui::widgets::track_list::{render_track_list, TrackListOpts, TrackListState};
use crate::ui::HitMap;
use ratatui::prelude::*;
use unicode_width::UnicodeWidthStr;

pub enum DetailKind<'a> {
    Album(&'a Album),
    Artist(&'a Artist),
    Playlist(&'a Playlist),
}

/// Hero header (cover, kind, title, metadata, action pills) above the track table.
pub fn render(
    kind: DetailKind<'_>,
    list: &TrackListState,
    playing_id: Option<&str>,
    cover: CoverCtx<'_>,
    area: Rect,
    buf: &mut Buffer,
    theme: &Theme,
    hit: &mut HitMap,
) {
    let (label, title, meta, tracks): (&str, String, String, &[Track]) = match kind {
        DetailKind::Album(a) => {
            let n = a.tracks.len();
            ("ALBUM", a.title.clone(), format!("{} · {n} songs", artist_line(a)), &a.tracks)
        }
        DetailKind::Artist(a) => (
            "ARTIST",
            a.name.clone(),
            format!("Top songs · {}", a.top_songs.len()),
            &a.top_songs,
        ),
        DetailKind::Playlist(p) => {
            let count = p.track_count.unwrap_or(p.tracks.len() as u32);
            let meta = match p.author.as_deref() {
                Some(a) if !a.is_empty() => format!("{a} · {count} songs"),
                _ => format!("{count} songs"),
            };
            ("PLAYLIST", p.title.clone(), meta, &p.tracks)
        }
    };

    let hero_h: u16 = if area.height >= 26 {
        8
    } else if area.height >= 16 {
        6
    } else {
        4
    };
    let hero = Rect {
        height: hero_h.min(area.height),
        ..area
    };
    hit.inert.push(hero);
    let art_w = cover.width_for_rows(hero.height);
    let show_art = area.width >= art_w + 30;
    let text_x = if show_art {
        let r = cover.draw(
            CoverSlot::Detail,
            Rect {
                width: art_w,
                ..hero
            },
            buf,
            theme,
        );
        r.x + r.width + 3
    } else {
        hero.x
    };
    let text_w = (hero.x + hero.width).saturating_sub(text_x) as usize;

    // Bottom-align the text block against the cover.
    let lines = 4u16;
    let ty = hero.y + hero.height.saturating_sub(lines + 1).min(hero.height.saturating_sub(lines));
    if text_w >= 10 {
        buf.set_stringn(text_x, ty, label, text_w, theme.subheading());
        buf.set_stringn(text_x, ty + 1, truncate(&title, text_w), text_w, theme.heading());
        buf.set_stringn(text_x, ty + 2, truncate(&meta, text_w), text_w, theme.dim());
        if hero.height >= 5 {
            let mut x = text_x;
            let y = ty + 3;
            let actions = [
                ("▶ Play", Action::PlayAll, true),
                ("◉ Radio", Action::StartRadio, false),
            ];
            for (lbl, action, primary) in actions {
                let w = lbl.width() as u16 + 2;
                if x + w > text_x + text_w as u16 {
                    break;
                }
                let r = pill(buf, x, y + 1, lbl, primary, theme);
                hit.buttons.push((r, action));
                x += w + 1;
            }
        }
    }

    let list_y = hero.y + hero.height + 1;
    let list_area = Rect {
        y: list_y,
        height: (area.y + area.height).saturating_sub(list_y),
        ..area
    };
    render_track_list(
        tracks,
        list,
        TrackListOpts {
            playing_id,
            ..Default::default()
        },
        list_area,
        buf,
        theme,
        Some(&mut hit.list_rows),
    );
}
