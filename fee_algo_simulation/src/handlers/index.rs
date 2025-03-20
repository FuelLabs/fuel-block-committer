use super::*;

pub async fn index_html() -> HttpResponse {
    let contents = include_str!("index.html");
    HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .body(contents)
}
