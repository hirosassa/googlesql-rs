fn main() -> Result<(), googlesql::Error> {
    let mut m = googlesql::Module::new()?;
    for sql in [
        "SELECT 1",
        "SELECT 1 + 2 AS x",
        "SELECT x FROM missing_table",
        "SELECT FROM",
    ] {
        match m.analyze_statement(sql) {
            Ok(()) => println!("IN : {sql}\nOK : analysis succeeded\n"),
            Err(e) => println!("IN : {sql}\nERR: {e}\n"),
        }
    }
    Ok(())
}
