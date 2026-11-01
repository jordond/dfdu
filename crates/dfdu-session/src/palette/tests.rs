use super::*;

#[test]
fn token_names_round_trip() {
    for (i, t) in Token::ALL.into_iter().enumerate() {
        assert_eq!(t as usize, i, "{t:?} is in declaration order");
        assert_eq!(Token::from_name(t.name()), Some(t));
    }
    assert_eq!(Token::from_name("cursor.bg"), None);
    for c in Category::ALL {
        assert_eq!(
            c.token().name().strip_prefix("cat."),
            Some(format!("{c:?}").to_lowercase().as_str())
        );
    }
    for s in SafetyStyle::ALL {
        assert_eq!(
            s.token().name().strip_prefix("safety."),
            Some(format!("{s:?}").to_lowercase().as_str())
        );
    }
}

#[test]
fn palette_is_indexed_by_token() {
    let p = Palette::from_fn(|t| {
        Spec::fg(ColorSpec {
            ansi256: Some(t as u8),
            ..ColorSpec::NONE
        })
    });
    for t in Token::ALL {
        assert_eq!(p[t].fg.ansi256, Some(t as u8), "{t:?}");
    }
    let mut p = Palette::NONE;
    p[Token::Dir] = Spec::NONE.add(Modifiers::BOLD);
    assert_eq!(p[Token::Dir].modifiers, Modifiers::BOLD);
    assert_eq!(p[Token::Key], Spec::NONE);
}
