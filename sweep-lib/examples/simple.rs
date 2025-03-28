use std::sync::LazyLock;

use anyhow::Error;
use surf_n_term::Glyph;
use sweep::{
    Haystack, HaystackTagged, Sweep, SweepOptions,
    surf_n_term::{CellWrite, view::Text},
};

static YES: LazyLock<Glyph> = LazyLock::new(|| {
    let glyph_str = r#"{
        "path": "M44.8,8.85Q61.2,8.35 70.55,10.9Q79.9,13.45 84.6,19.75Q89,25.65 90.5,36.55Q91.1,41.15 91.3,46.95Q91.6,57.55 90.1,65.85Q88.1,77.25 82.8,82.55Q79.5,85.85 74.85,87.8Q70.2,89.75 63.5,90.55Q59.4,91.05 53.8,91.25Q43,91.65 34.6,90.25Q22.9,88.15 17.5,82.85Q15.3,80.65 13.9,78.25Q11.4,73.85 10.1,67.15Q8.4,58.55 8.8,46.15Q9,36.55 10.5,30.55Q12.5,22.35 17.4,17.45Q21.6,13.35 28.1,11.3Q34.6,9.25 44.8,8.85ZM50,83.05Q60.7,83.05 66.6,81.75Q73.2,80.35 76.7,76.85Q80.2,73.35 81.6,66.75Q83,60.75 83,50.05Q83,40.25 81.8,34.25Q80.6,28.15 77.95,24.7Q75.3,21.25 70.6,19.45Q66.4,17.95 59.2,17.35Q52.2,16.85 45,17.15Q31.8,17.65 26,21.15Q22.7,23.15 20.75,26.65Q18.8,30.15 17.9,35.75Q17,41.35 17,50.05Q17,60.95 18.5,67.25Q20,73.55 23.3,76.85Q26.3,79.75 31.7,81.25Q38.2,83.05 50,83.05ZM40.4,29.65Q39.1,30.05 38.3,31.2Q37.5,32.35 37.6,33.85Q37.6,34.25 38,35.25L45.8,54.85L45.8,60.95Q45.8,65.85 45.9,66.85Q46,67.85 46.4,68.55Q46.8,69.25 47.4,69.75Q48.5,70.65 50,70.65Q51.5,70.65 52.6,69.75Q53.2,69.25 53.6,68.55Q54,67.85 54.1,66.85Q54.2,65.85 54.2,60.95L54.2,54.85L62,35.25Q62.4,34.25 62.4,33.85Q62.5,32.25 61.5,30.95Q60.3,29.45 58,29.45Q56.9,29.45 55.8,30.25Q54.9,30.95 54.5,31.75L50,42.95L47.8,37.55Q46.3,33.75 45.8,32.65Q45.2,31.15 44.75,30.7Q44.3,30.25 43.5,29.85Q43,29.55 42.7,29.5Q42.4,29.45 41.8,29.45Q41.2,29.45 40.4,29.65Z",
        "view_box": [0, 0, 100, 100],
        "size": [1, 2]
    }"#;
    serde_json::from_str(glyph_str).unwrap()
});
static NO: LazyLock<Glyph> = LazyLock::new(|| {
    let glyph_str = r#"{
        "path": "M44.8,8.85Q61.2,8.35 70.55,10.9Q79.9,13.45 84.6,19.75Q89,25.65 90.5,36.55Q91.1,41.15 91.3,46.95Q91.6,57.55 90.1,65.85Q88.1,77.25 82.8,82.55Q79.5,85.85 74.85,87.8Q70.2,89.75 63.5,90.55Q59.4,91.05 53.8,91.25Q43,91.65 34.6,90.25Q22.9,88.15 17.5,82.85Q15.3,80.65 13.9,78.25Q11.4,73.85 10.1,67.15Q8.4,58.55 8.8,46.15Q9,36.55 10.5,30.55Q12.5,22.35 17.4,17.45Q21.6,13.35 28.1,11.3Q34.6,9.25 44.8,8.85ZM50,83.05Q60.7,83.05 66.6,81.75Q73.2,80.35 76.7,76.85Q80.2,73.35 81.6,66.75Q83,60.75 83,50.05Q83,40.25 81.8,34.25Q80.6,28.15 77.95,24.7Q75.3,21.25 70.6,19.45Q66.4,17.95 59.2,17.35Q52.2,16.85 45,17.15Q31.8,17.65 26,21.15Q22.7,23.15 20.75,26.65Q18.8,30.15 17.9,35.75Q17,41.35 17,50.05Q17,60.95 18.5,67.25Q20,73.55 23.3,76.85Q26.3,79.75 31.7,81.25Q38.2,83.05 50,83.05ZM40.4,29.65Q39.5,29.95 38.8,30.7Q38.1,31.45 37.8,32.35Q37.7,32.75 37.6,35.05L37.6,64.95Q37.7,67.35 37.8,67.75L37.8,67.75Q38.9,70.65 42,70.65Q43.3,70.65 44.1,69.95Q44.7,69.45 45.8,67.65L45.8,51.05L53.8,66.85Q54.8,68.85 55.3,69.45Q55.7,69.85 56.4,70.15L56.5,70.25Q57.3,70.65 58,70.65Q61.1,70.65 62.2,67.75Q62.3,67.35 62.3,64.95L62.3,35.05Q62.3,32.75 62.2,32.35L62.2,32.35Q61.1,29.45 58,29.45Q56.7,29.45 55.9,30.15Q55.3,30.65 54.2,32.45L54.2,49.05L46.2,33.25Q45.1,31.25 44.6,30.65Q44.3,30.15 43.6,29.95L43.5,29.85Q43,29.55 42.7,29.5Q42.4,29.45 41.8,29.45Q41.2,29.45 40.4,29.65Z",
        "view_box": [0, 0, 100, 100],
        "size": [1, 2]
    }"#;
    serde_json::from_str(glyph_str).unwrap()
});

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Error> {
    let options = SweepOptions {
        // tty_path: "/dev/pts/8".to_owned(),
        ..Default::default()
    };
    let sweep = Sweep::<HaystackTagged<Text, &'static str>>::new((), options)?;
    sweep.items_extend(
        None,
        [Text::new()
            .put_fmt("Confirm Y/N", None)
            .take()
            .tagged("confirm", Some("1".parse()?))],
    );
    while let Some(event) = sweep.next_event().await {
        if let sweep::SweepEvent::Select { items, .. } = event {
            let Some(item) = items.first() else {
                continue;
            };
            if item.tag == "confirm" && yes_or_no(&sweep).await? {
                break;
            }
        }
    }
    Ok(())
}

async fn yes_or_no<H: Haystack>(sweep: &Sweep<H>) -> Result<bool, Error> {
    let result = sweep
        .quick_select(
            Some(SweepOptions {
                prompt: "Y/N".to_owned(),
                theme: "accent=gruv-red-2".parse()?,
                ..Default::default()
            }),
            "yes/no".into(),
            (),
            [
                Text::new()
                    .by_ref()
                    .with_face("fg=gruv-red-2,bold".parse()?)
                    .with_glyph(YES.clone())
                    .put_fmt("es", None)
                    .take()
                    .tagged(true, Some("y".parse()?)),
                Text::new()
                    .by_ref()
                    .with_face("fg=gruv-green-2,bold".parse()?)
                    .with_glyph(NO.clone())
                    .put_fmt("o", None)
                    .take()
                    .tagged(false, Some("n".parse()?)),
            ],
        )
        .await?;
    Ok(result
        .into_iter()
        .next()
        .map_or_else(|| false, |item| item.tag))
}
