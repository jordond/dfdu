use super::*;

#[test]
fn modifiers_map_one_to_one() {
    let all = Modifiers::ALL
        .into_iter()
        .fold(Modifiers::empty(), |acc, m| acc | m);
    assert_eq!(modifier(all), Modifier::all());
    assert_eq!(modifier(Modifiers::empty()), Modifier::empty());
    for m in Modifiers::ALL {
        assert_eq!(modifier(m).bits().count_ones(), 1, "{m:?}");
    }
    assert_eq!(
        modifier(Modifiers::BOLD | Modifiers::UNDERLINED),
        Modifier::BOLD | Modifier::UNDERLINED
    );
}

#[test]
fn builtin_styles_keep_their_values() {
    use ratatui::style::Color;
    let t = Theme::ansi16();
    assert_eq!(t.title, Style::new().add_modifier(Modifier::BOLD));
    assert_eq!(t.fg_dim, Style::new().fg(Color::DarkGray));
    assert_eq!(t.bar_fill, Style::new().fg(Color::Cyan));
    assert_eq!(
        t.dir,
        Style::new().fg(Color::Blue).add_modifier(Modifier::BOLD)
    );
    assert_eq!(t.cursor, Style::new().add_modifier(Modifier::REVERSED));
    assert_eq!(t.status_warn, Style::new().fg(Color::Yellow));
    assert_eq!(t.status_error, Style::new().fg(Color::Red));
    assert_eq!(t.key, Style::new().add_modifier(Modifier::BOLD));
    assert_eq!(
        (t.name(), t.level(), t.background()),
        ("ansi", ColorLevel::Ansi16, None)
    );

    let m = Theme::no_color();
    assert_eq!(m.title, Style::new().add_modifier(Modifier::BOLD));
    assert_eq!(m.fg_dim, Style::new().add_modifier(Modifier::DIM));
    assert_eq!(m.bar_fill, Style::new());
    assert_eq!(m.dir, Style::new().add_modifier(Modifier::BOLD));
    assert_eq!(m.cursor, Style::new().add_modifier(Modifier::REVERSED));
    assert_eq!(m.status_warn, Style::new().add_modifier(Modifier::BOLD));
    assert_eq!(m.status_error, Style::new().add_modifier(Modifier::BOLD));
    assert_eq!(m.key, Style::new().add_modifier(Modifier::BOLD));
    assert_eq!((m.name(), m.level()), ("mono", ColorLevel::None));
}

#[test]
fn named_themes_remember_how_they_were_resolved() {
    for &name in Theme::builtin_names() {
        let t = Theme::named(name, ColorLevel::TrueColor, Some((1, 2, 3))).expect(name);
        assert_eq!(t.name(), name);
        assert_eq!(t.background(), Some((1, 2, 3)));
        let want = if name == "mono" {
            ColorLevel::None
        } else {
            ColorLevel::TrueColor
        };
        assert_eq!(t.level(), want, "{name}");
    }
    assert!(Theme::named("solarized", ColorLevel::TrueColor, None).is_none());
}

#[test]
fn every_token_resolves_into_its_own_field() {
    use ratatui::style::Color;
    let palette = Palette::from_fn(|t| {
        Spec::fg(ColorSpec {
            ansi256: Some(t as u8),
            ..ColorSpec::NONE
        })
    });
    let theme = Theme::resolve("test".into(), &palette, ColorLevel::Ansi256, None);
    for t in Token::ALL {
        assert_eq!(theme.style(t).fg, Some(Color::Indexed(t as u8)), "{t:?}");
    }
    assert_eq!(theme.cursor, theme.style(Token::Cursor));
    assert_eq!(theme.r#match, theme.style(Token::Match));
    assert_eq!(
        theme.cat[Category::Media as usize],
        theme.style(Token::CatMedia)
    );
    assert_eq!(
        theme.safety[SafetyStyle::Keep as usize],
        theme.style(Token::SafetyKeep)
    );
}
