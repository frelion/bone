use std::path::Path;
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let after = if args[3] == "-" { None } else { Some(args[3].as_str()) };
    let page = bone::history_page(Path::new(&args[1]), &args[2], after, args[4].parse().unwrap());
    match page {
        Ok(page) => println!("{}", serde_json::to_string(&page).unwrap()),
        Err(error) => { eprintln!("{error:#}"); std::process::exit(1); }
    }
}
