use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut raw = std::env::args_os().skip(1).peekable();
    if raw
        .peek()
        .is_some_and(|argument| argument == "--extract-black-matte")
    {
        let _ = raw.next();
        let input = raw
            .next()
            .map(PathBuf::from)
            .ok_or("用法：frd-icon-assets --extract-black-matte <黑底源 PNG> <透明输出 PNG>")?;
        let output = raw
            .next()
            .map(PathBuf::from)
            .ok_or("用法：frd-icon-assets --extract-black-matte <黑底源 PNG> <透明输出 PNG>")?;
        if raw.next().is_some() {
            return Err("参数数量无效".into());
        }
        return frd_icon_assets::extract_black_matte(&input, &output);
    }
    let mut arguments = raw.map(PathBuf::from);
    let source = arguments
        .next()
        .ok_or("用法：frd-icon-assets <透明前景 PNG> <输出目录>")?;
    let output = arguments
        .next()
        .ok_or("用法：frd-icon-assets <透明前景 PNG> <输出目录>")?;
    if arguments.next().is_some() {
        return Err("参数数量无效".into());
    }
    frd_icon_assets::export_assets(&source, &output)
}
