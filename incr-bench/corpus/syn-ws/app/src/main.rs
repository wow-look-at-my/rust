use syn::fold::Fold;
use syn::visit::Visit;
use syn::visit_mut::VisitMut;

struct IdentCounter(usize);

impl<'ast> Visit<'ast> for IdentCounter {
    fn visit_ident(&mut self, _: &'ast syn::Ident) {
        self.0 += 1;
    }
}

struct Identity;

impl Fold for Identity {}

struct DropAttrs;

impl VisitMut for DropAttrs {
    fn visit_attributes_mut(&mut self, attrs: &mut Vec<syn::Attribute>) {
        attrs.clear();
    }
}

const SAMPLE: &str = r#"
#[derive(Debug)]
struct Point { x: i32, y: i32 }
impl Point { fn norm(&self) -> i32 { self.x * self.x + self.y * self.y } }
fn main() { let p = Point { x: 1, y: 2 }; println!("{}", p.norm()); }
"#;

fn analyze(src: &str) -> syn::Result<(usize, usize)> {
    let file: syn::File = syn::parse_str(src)?;
    let mut counter = IdentCounter(0);
    counter.visit_file(&file);
    let mut folded = Identity.fold_file(file.clone());
    DropAttrs.visit_file_mut(&mut folded);
    Ok((counter.0, format!("{:?}", folded).len()))
}

fn main() {
    let src = match std::env::args().nth(1) {
        Some(path) => std::fs::read_to_string(path).expect("read input file"),
        None => SAMPLE.to_string(),
    };
    let (idents, debug_len) = analyze(&src).expect("parse input");
    println!("idents={idents} debug_len={debug_len}");
}

#[cfg(test)]
mod tests {
    #[test]
    fn sample_parses() {
        let (idents, _) = super::analyze(super::SAMPLE).unwrap();
        assert!(idents > 10);
    }
}
