# 资产与第三方声明

本仓库的代码与文档默认按 MIT OR Apache-2.0 双许可发布，使用者可以任选其一，
详见 [LICENSE-MIT](LICENSE-MIT) 与 [LICENSE-APACHE](LICENSE-APACHE)。

以下资产需要单独说明。

## 程序化生成的示例资产

`assets/` 下的 GLB、GLTF、HDR、IPAK 与关卡档，以及 `docs/` 下的离屏验收图，
由仓库内示例程序生成，或由项目作者编写。对应的生成器包括：

- `crates/iris-engine/examples/gen_asset.rs`
- `crates/iris-engine/examples/gen_r59_assets.rs`
- `games/iris-assault/examples/gen_assault_assets.rs`
- 各游戏与渲染示例的离屏快照程序

这些资产随本仓库按 MIT OR Apache-2.0 提供。重新生成时以示例程序的输出为准。

## 外部角色模型截图

`docs/exp.png`、`docs/exp_closeup.png`、`docs/exp_crown.png` 与
`docs/exp_side.png` 是实验项目 `games/iris-exp` 使用外部角色模型生成的
渲染验收截图。角色模型本体 `assets/exp_weiqing.glb` 因版权原因未随仓库
发布，`.gitignore` 已将其排除。

这些截图的版权与使用条件归角色模型的原权利人所有。仓库仅用它们展示
引擎的 glTF 加载、蒙皮动画与材质能力，不包含模型本体，也不构成对模型的
再授权。使用、复制或再分发这些截图前，应自行确认原权利人的许可条件。

## 位图字体

UI 使用的 font8x8 字形表属于公有领域，来源与图集布局记录在
[ARCHITECTURE.md](ARCHITECTURE.md) 的 UI 相关章节，并由单元测试守护。

## 第三方依赖

Rust 依赖的许可证以各 crate 的 `Cargo.toml` 与发布包中的许可证声明为准，
`Cargo.lock` 记录实际使用的版本。本仓库不复制第三方 crate 的源代码。
