use axum::response::Html;

pub fn to(url: &str) -> Html<String> {
    let str = include_str!("../assets/redirect.html").replace("%REDIRECT_URL%", url);

    Html(str)
}
