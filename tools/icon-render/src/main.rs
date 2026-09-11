//! 图标渲染小工具：将 SVG 光栅化为 PNG，用于生成应用的各尺寸图标。
//! 用法：`icon-render <input.svg> <output.png>`，依赖系统字体渲染 SVG 中的文本。

use std::path::Path;

/// 读取命令行指定的 SVG，经 usvg 布局后用 resvg/tiny-skia 渲染并保存为 PNG。
fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: icon-render <input.svg> <output.png>");
        std::process::exit(1);
    }
    let svg = std::fs::read_to_string(Path::new(&args[1])).expect("read svg");
    // 加载系统字体供 SVG 中的 <text> 元素使用
    let mut fontdb = fontdb::Database::new();
    fontdb.load_system_fonts();
    let mut options = usvg::Options::default();
    options.fontdb = std::sync::Arc::new(fontdb);
    let tree = usvg::Tree::from_str(&svg, &options).expect("parse svg");
    let size = tree.size();
    let mut pixmap = tiny_skia::Pixmap::new(size.width() as u32, size.height() as u32)
        .expect("create pixmap");
    resvg::render(&tree, tiny_skia::Transform::default(), &mut pixmap.as_mut());
    pixmap.save_png(Path::new(&args[2])).expect("save png");
    println!(
        "rendered {}x{} -> {}",
        size.width() as u32,
        size.height() as u32,
        args[2]
    );
}
