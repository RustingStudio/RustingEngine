//! Visual shader graphs: nodes wired into a graph that compiles to
//! `#rusting surface 1` GLSL, the same text a material's `shader` takes.
//!
//! Every node yields a `vec4`, so any output plugs into any input. A node
//! reads only nodes before it in the list, which keeps the graph acyclic
//! and the GLSL in node order.

use serde::{Deserialize, Serialize};

/// A value the pixel already has: a `SurfaceInput` field or the surface's
/// value before the graph runs. Scalars fill all four lanes; vectors pad
/// with zeros.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShaderInput {
    Uv,
    WorldPosition,
    ViewDirection,
    VertexColor,
    Time,
    BaseColor,
    Normal,
    Metallic,
    Roughness,
    Emissive,
}

/// A `Surface` field the graph writes. Vector fields take the first lanes
/// of the value; `Metallic` and `Roughness` take `x`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SurfaceOutput {
    BaseColor,
    Normal,
    Metallic,
    Roughness,
    Emissive,
}

/// One node. Indices name earlier nodes in [`ShaderGraph::nodes`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ShaderNode {
    Input(ShaderInput),
    Constant([f32; 4]),
    Add(usize, usize),
    Subtract(usize, usize),
    Multiply(usize, usize),
    /// `mix(a, b, t)` per lane.
    Mix(usize, usize, usize),
    Sin(usize),
    Fract(usize),
    /// One lane (0 to 3) of a value, in all four lanes.
    Component(usize, u8),
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ShaderGraph {
    pub nodes: Vec<ShaderNode>,
    /// Surface fields and the node each one takes, applied in order.
    pub outputs: Vec<(SurfaceOutput, usize)>,
}

impl ShaderGraph {
    /// GLSL for a material's `shader`, or why the graph is invalid.
    pub fn compile(&self) -> Result<String, String> {
        let mut glsl = String::from(
            "#rusting surface 1\nvoid surface(SurfaceInput i, inout Surface s) {\n",
        );
        for (index, node) in self.nodes.iter().enumerate() {
            let at = |input: usize| {
                if input < index {
                    Ok(format!("n{input}"))
                } else {
                    Err(format!(
                        "node {index} reads node {input}, which does not come before it"
                    ))
                }
            };
            let expression = match *node {
                ShaderNode::Input(input) => match input {
                    ShaderInput::Uv => "vec4(i.uv, 0.0, 0.0)",
                    ShaderInput::WorldPosition => "vec4(i.world_position, 0.0)",
                    ShaderInput::ViewDirection => "vec4(i.view_direction, 0.0)",
                    ShaderInput::VertexColor => "i.vertex_color",
                    ShaderInput::Time => "vec4(i.time)",
                    ShaderInput::BaseColor => "s.base_color",
                    ShaderInput::Normal => "vec4(s.normal, 0.0)",
                    ShaderInput::Metallic => "vec4(s.metallic)",
                    ShaderInput::Roughness => "vec4(s.roughness)",
                    ShaderInput::Emissive => "vec4(s.emissive, 0.0)",
                }
                .to_owned(),
                ShaderNode::Constant(value) => {
                    if value.iter().any(|lane| !lane.is_finite()) {
                        return Err(format!(
                            "node {index} is not a finite number"
                        ));
                    }
                    let [x, y, z, w] = value;
                    format!("vec4({x:?}, {y:?}, {z:?}, {w:?})")
                }
                ShaderNode::Add(a, b) => format!("{} + {}", at(a)?, at(b)?),
                ShaderNode::Subtract(a, b) => {
                    format!("{} - {}", at(a)?, at(b)?)
                }
                ShaderNode::Multiply(a, b) => {
                    format!("{} * {}", at(a)?, at(b)?)
                }
                ShaderNode::Mix(a, b, t) => {
                    format!("mix({}, {}, {})", at(a)?, at(b)?, at(t)?)
                }
                ShaderNode::Sin(a) => format!("sin({})", at(a)?),
                ShaderNode::Fract(a) => format!("fract({})", at(a)?),
                ShaderNode::Component(a, lane) => {
                    let lane =
                        ["x", "y", "z", "w"].get(usize::from(lane)).ok_or(
                            format!("node {index} reads lane {lane} of 0 to 3"),
                        )?;
                    format!("vec4({}.{lane})", at(a)?)
                }
            };
            glsl += &format!("    vec4 n{index} = {expression};\n");
        }
        for &(output, node) in &self.outputs {
            if node >= self.nodes.len() {
                return Err(format!("{output:?} reads missing node {node}"));
            }
            glsl += &match output {
                SurfaceOutput::BaseColor => {
                    format!("    s.base_color = n{node};\n")
                }
                SurfaceOutput::Normal => {
                    format!("    s.normal = n{node}.xyz;\n")
                }
                SurfaceOutput::Metallic => {
                    format!("    s.metallic = n{node}.x;\n")
                }
                SurfaceOutput::Roughness => {
                    format!("    s.roughness = n{node}.x;\n")
                }
                SurfaceOutput::Emissive => {
                    format!("    s.emissive = n{node}.xyz;\n")
                }
            };
        }
        Ok(glsl + "}\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graphs_compile_in_node_order_and_reject_bad_wiring() {
        let graph = ShaderGraph {
            nodes: vec![
                ShaderNode::Input(ShaderInput::Time),
                ShaderNode::Sin(0),
                ShaderNode::Constant([0.5, 1.0, 0.0, 1.0]),
                ShaderNode::Multiply(1, 2),
            ],
            outputs: vec![
                (SurfaceOutput::Emissive, 3),
                (SurfaceOutput::Roughness, 2),
            ],
        };
        assert_eq!(
            graph.compile().unwrap(),
            "#rusting surface 1\n\
             void surface(SurfaceInput i, inout Surface s) {\n    \
             vec4 n0 = vec4(i.time);\n    \
             vec4 n1 = sin(n0);\n    \
             vec4 n2 = vec4(0.5, 1.0, 0.0, 1.0);\n    \
             vec4 n3 = n1 * n2;\n    \
             s.emissive = n3.xyz;\n    \
             s.roughness = n2.x;\n\
             }\n"
        );
        // A graph saves and loads as plain data.
        let json = serde_json::to_string(&graph).unwrap();
        assert_eq!(serde_json::from_str::<ShaderGraph>(&json).unwrap(), graph);
        let bad = |nodes: Vec<ShaderNode>, output: usize| {
            ShaderGraph {
                nodes,
                outputs: vec![(SurfaceOutput::BaseColor, output)],
            }
            .compile()
            .unwrap_err()
        };
        let constant = || ShaderNode::Constant([0.0; 4]);
        assert!(
            bad(vec![ShaderNode::Sin(0)], 0).contains("node 0 reads node 0")
        );
        assert!(bad(vec![constant()], 1).contains("missing node 1"));
        assert!(bad(vec![constant(), ShaderNode::Component(0, 4)], 1)
            .contains("lane 4"));
        assert!(bad(vec![ShaderNode::Constant([f32::NAN; 4])], 0)
            .contains("finite"));
    }
}
