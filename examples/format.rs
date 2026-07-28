fn main() -> Result<(), googlesql::Error> {
    let mut m = googlesql::Module::new()?;
    for sql in [
        "select a,b from t where a>1",
        "select 1+2 as x",
        "SELECT FROM",
    ] {
        match m.format_sql(sql) {
            Ok(formatted) => println!("IN : {sql}\nOUT:\n{formatted}\n"),
            Err(e) => println!("IN : {sql}\nERR: {e}\n"),
        }
    }
    Ok(())
}
