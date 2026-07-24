use kscope::app::App;
use kscope::config::Config;
use kscope::k8s::discovery::{collect, Scope};
use ratatui::backend::TestBackend;
use ratatui::Terminal;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let (client, _ns, ctx, user) = kscope::k8s::connect(Some("docker-desktop")).await?;
    let version = kscope::k8s::server_version(&client).await;
    let scope = Scope::Namespace("default".into());
    let inv = collect(&client, &scope).await?;

    let (log_tx, _log_rx) = tokio::sync::mpsc::channel(16);
    let mut app = App::new(Config::default(), client, scope, log_tx, ctx, user, version);
    app.inventory = inv;
    app.rebuild_sidebar();

    let backend = TestBackend::new(120, 40);
    let mut terminal = Terminal::new(backend)?;
    terminal.draw(|f| kscope::ui::draw(f, &mut app))?;

    let buf = terminal.backend().buffer().clone();
    for y in 0..buf.area.height {
        let mut line = String::new();
        for x in 0..buf.area.width {
            line.push_str(buf.get(x, y).symbol());
        }
        println!("{line}");
    }
    Ok(())
}
