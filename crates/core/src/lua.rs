use mlua::prelude::*;

pub struct LuaEngine {
    lua: Lua,
}

impl LuaEngine {
    pub fn new() -> LuaResult<Self> {
        let lua = Lua::new_with(LuaStdLib::ALL_SAFE, LuaOptions::default())?;
        register_pc_table(&lua)?;
        Ok(Self { lua })
    }

    pub fn exec(&self, code: &str) -> LuaResult<()> {
        self.lua.load(code).exec()
    }
}

fn register_pc_table(lua: &Lua) -> LuaResult<()> {
    let pc = lua.create_table()?;

    // pc.run(cmd [, args_table])
    pc.set(
        "run",
        lua.create_function(|_, (cmd, args): (String, Option<Vec<String>>)| {
            let args = args.unwrap_or_default();
            std::process::Command::new(&cmd)
                .args(&args)
                .spawn()
                .map_err(LuaError::external)?;
            Ok(())
        })?,
    )?;

    // pc.url(url)
    pc.set(
        "url",
        lua.create_function(|_, url: String| {
            open::that(&url).map_err(LuaError::external)?;
            Ok(())
        })?,
    )?;

    // pc.open(path)
    pc.set(
        "open",
        lua.create_function(|_, path: String| {
            open::that(&path).map_err(LuaError::external)?;
            Ok(())
        })?,
    )?;

    // pc.log(msg)
    pc.set(
        "log",
        lua.create_function(|_, msg: String| {
            log::info!("[lua] {msg}");
            Ok(())
        })?,
    )?;

    lua.globals().set("pc", pc)?;
    Ok(())
}
