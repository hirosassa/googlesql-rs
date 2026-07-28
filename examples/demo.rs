fn main() -> Result<(), googlesql::Error> {
    let mut m = googlesql::Module::new()?;
    for sql in ["select 1", "SELECT a,b FROM t WHERE a>1", "select 1+2 as x"] {
        match m.parse_statement(sql) {
            Ok(p) => println!("IN : {sql}\nOUT: {:?}\n", p.canonical_sql()),
            Err(e) => println!("IN : {sql}\nERR: {e}\n"),
        }
    }
    Ok(())
}
