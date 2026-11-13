//! News: what the people who run this Hearth tell everyone on it.

use hearth_api::{
    Id,
    community::{NewsPatch, NewsPost, PublishNews},
};
use leptos::{prelude::*, task::spawn_local};
use pulldown_cmark::{Event, Parser, Tag, html};

use crate::{
    App, api,
    ui::{Field, Icon, TextInput, Toggle, delete_then, load_list, toast, toast_err, when},
};

/// A post as HTML, for the preview. Raw HTML in a post comes out as the text it is, so a post
/// can never bring markup of its own, and links lead to the web or to mail, never to script.
fn render(markdown: &str) -> String {
    let safe = |url: &str| ["http://", "https://", "mailto:"].iter().any(|scheme| url.to_ascii_lowercase().starts_with(scheme));
    let events = Parser::new(markdown).map(|event| match event {
        Event::Html(raw) | Event::InlineHtml(raw) => Event::Text(raw),
        Event::Start(Tag::Link { link_type, dest_url, title, id }) if !safe(&dest_url) => Event::Start(Tag::Link { link_type, dest_url: "#".into(), title, id }),
        Event::Start(Tag::Image { link_type, dest_url, title, id }) if !safe(&dest_url) => Event::Start(Tag::Image { link_type, dest_url: "".into(), title, id }),
        other => other,
    });
    let mut out = String::new();
    html::push_html(&mut out, events);
    out
}

#[component]
pub fn News() -> impl IntoView {
    let app = expect_context::<App>();
    let posts = RwSignal::new(Vec::<NewsPost>::new());
    let (title, body) = (RwSignal::new(String::new()), RwSignal::new(String::new()));
    let pinned = RwSignal::new(false);
    let preview = RwSignal::new(false);
    let load = move || load_list("/news", posts, "The posts could not be loaded");
    load();

    let ready = move || !title.with(|title| title.trim().is_empty()) && !body.with(|body| body.trim().is_empty());
    let publish = move |_| {
        let post = PublishNews { title: title.get_untracked(), body: body.get_untracked(), pinned: pinned.get_untracked() };
        let hearth = app.name();
        spawn_local(async move {
            match api::post::<NewsPost>("/admin/news", &post).await {
                Ok(_) => {
                    title.set(String::new());
                    body.set(String::new());
                    pinned.set(false);
                    preview.set(false);
                    toast("ok", "Published", format!("Everyone on {hearth} was notified."));
                    load();
                }
                Err(err) => toast_err("Not published", &err),
            }
        });
    };
    let pin = move |id: Id, on: bool| {
        spawn_local(async move {
            match api::patch::<()>(&format!("/admin/news/{id}"), &NewsPatch { pinned: Some(on), ..Default::default() }).await {
                Ok(()) => load(),
                Err(err) => toast_err("Not saved", &err),
            }
        });
    };

    let editor = move || {
        if !preview.get() {
            return view! {
                <Field label="Title" target="news-title">
                    <TextInput value=title id="news-title" placeholder="Stardew Valley 1.6.15 is on the Hearth"/>
                </Field>
                <Field label="Post" target="news-post">
                    <textarea
                        class="input"
                        id="news-post"
                        rows="6"
                        placeholder="What changed, and what people should do. Markdown works."
                        prop:value=move || body.get()
                        on:input=move |ev| body.set(event_target_value(&ev))
                    ></textarea>
                </Field>
            }
                .into_any();
        }
        let heading = Some(title.get()).filter(|title| !title.trim().is_empty()).unwrap_or_else(|| "Title".into());
        let author = app.me.get().map(|me| me.user.username).unwrap_or_default();
        let text = Some(body.get()).filter(|body| !body.trim().is_empty()).unwrap_or_else(|| "Your post shows up here.".into());
        view! {
            <div class="preview">
                <h4>{heading}</h4>
                <span class="muted" style="font-size:12px">{format!("{author} on {} · today", app.name())}</span>
                <div class="prose" inner_html=render(&text)></div>
            </div>
        }
            .into_any()
    };

    let published = move || {
        let posts = posts.get();
        if posts.is_empty() {
            return view! { <div class="empty">"Nothing published yet."</div> }.into_any();
        }
        posts
            .into_iter()
            .map(|post| {
                let (id, is_pinned) = (post.id, post.pinned);
                let detail = format!("{} · {}{}", post.author, when(post.published_at), if is_pinned { " · Pinned" } else { "" });
                view! {
                    <div class="row">
                        <div class="col">
                            <span class="t ell">{post.title}</span>
                            <span class="d">{detail}</span>
                        </div>
                        <button class="btn sm" on:click=move |_| pin(id, !is_pinned)>{if is_pinned { "Unpin" } else { "Pin" }}</button>
                        <button class="ib" aria-label="Delete post" on:click=move |_| delete_then(format!("/admin/news/{id}"), "The post was not deleted", load)>
                            <Icon name="trash" size=14/>
                        </button>
                    </div>
                }
            })
            .collect_view()
            .into_any()
    };

    view! {
        <div class="view">
            <div class="ph">
                <div>
                    <span class="lab">{move || format!("Shown in Bonfire to everyone on {}", app.name())}</span>
                    <h1 class="pt">"News"</h1>
                </div>
            </div>
            <div class="cols even">
                <div class="card">
                    <header>
                        <h3>"Write a post"</h3>
                        <span class="sp"></span>
                        <div class="seg">
                            <button aria-pressed=move || (!preview.get()).to_string() on:click=move |_| preview.set(false)>"Write"</button>
                            <button aria-pressed=move || preview.get().to_string() on:click=move |_| preview.set(true)>"Preview"</button>
                        </div>
                    </header>
                    <div class="pad col" style="gap:12px">
                        {editor}
                        <div class="hrow">
                            <Toggle checked=pinned label="Pin to the top"/>
                            <span>"Pin to the top"</span>
                            <span class="sp"></span>
                            <button class="btn pri" disabled=move || !ready() on:click=publish>"Publish"</button>
                        </div>
                        <span class="faint" style="font-size:12px">
                            {move || format!("Publishing also sends a notification to everyone on {}.", app.name())}
                        </span>
                    </div>
                </div>
                <div class="card">
                    <header><h3>"Published"</h3><span class="muted mono">{move || posts.with(Vec::len)}</span></header>
                    <div class="rows">{published}</div>
                </div>
            </div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn raw_html_in_a_post_is_shown_as_text() {
        let html = super::render("**bold** <b>raw</b>\n\n<script>alert(1)</script>");
        assert!(html.contains("<strong>bold</strong>"), "{html}");
        assert!(!html.contains("<b>") && !html.contains("<script"), "{html}");
        let links = super::render("[a](https://example.com) [b](JavaScript:alert(1)) ![c](data:text/html,x)");
        assert!(links.contains("https://example.com") && !links.to_lowercase().contains("javascript:") && !links.contains("data:"), "{links}");
    }
}
