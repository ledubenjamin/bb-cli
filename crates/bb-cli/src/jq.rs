use anyhow::{Result, anyhow, bail};
use jaq_core::{
    Compiler, Ctx, Vars, data,
    load::{Arena, File, Loader},
    unwrap_valr,
};
use jaq_json::Val;

type Program = jaq_core::Filter<data::JustLut<Val>>;

fn compile(program: &str) -> Result<Program> {
    let defs = jaq_core::defs()
        .chain(jaq_std::defs())
        .chain(jaq_json::defs())
        .filter(|definition| {
            !matches!(definition.name, "stderr" | "debug" | "halt" | "halt_error")
        });
    let funs = jaq_core::funs::<data::JustLut<Val>>()
        .chain(jaq_std::funs())
        .chain(jaq_json::funs())
        .filter(|(name, _, _)| !matches!(*name, "env" | "debug_empty" | "stderr_empty" | "halt"));
    let arena = Arena::default();
    let modules = Loader::new(defs)
        .load(
            &arena,
            File {
                code: program,
                path: (),
            },
        )
        .map_err(|error| anyhow!("invalid jq program: {error:?}"))?;
    Compiler::default()
        .with_funs(funs)
        .compile(modules)
        .map_err(|error| anyhow!("invalid jq program: {error:?}"))
}

pub fn validate(program: &str) -> Result<()> {
    compile(program).map(|_| ())
}

pub fn filter(value: serde_json::Value, program: &str) -> Result<Vec<serde_json::Value>> {
    let compiled = compile(program)?;
    let input: Val = serde_json::from_value(value)?;
    let context = Ctx::<data::JustLut<Val>>::new(&compiled.lut, Vars::new([]));
    tracing::debug!(name: "output.jq.evaluate", "evaluating jq filter");
    let mut results = Vec::new();
    for result in compiled.id.run((context, input)).map(unwrap_valr) {
        if results.len() >= 100_000 {
            bail!("jq emitted more than 100000 results; narrow the filter");
        }
        let value = result.map_err(|error| anyhow!("jq evaluation failed: {error}"))?;
        results.push(serde_json::from_str(&value.to_string())?);
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn filters_cannot_read_environment_files_or_write_diagnostics() {
        for program in [
            "env",
            "$ENV",
            "debug",
            "stderr",
            "include \"private\"; .",
            "input",
            "halt",
            "halt_error(99)",
        ] {
            assert!(validate(program).is_err(), "{program}");
        }
    }

    #[test]
    fn permitted_filters_still_compile_and_transform_json() -> Result<()> {
        assert_eq!(
            filter(serde_json::json!([1, 2]), "map(. + 1)")?,
            vec![serde_json::json!([2, 3])]
        );
        Ok(())
    }
}
