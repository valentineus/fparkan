use crate::{read_f32, read_u32, MshError, NodeId, RawStream};

/// Model-local navigation points and links from MSH Res17.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Hallway {
    /// Points in native selection order.
    pub points: Vec<HallwayPoint>,
    /// Start and end point indexes for each authored link.
    pub links: Vec<[u32; 2]>,
}

/// A navigation point attached to a mesh node.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HallwayPoint {
    /// Position relative to the attached mesh node.
    pub position: [f32; 3],
    /// Raw point flags.
    pub flags: u32,
    /// Mesh node used for the world-space projection.
    pub node: NodeId,
}

pub(super) fn parse(stream: &RawStream) -> Result<Hallway, MshError> {
    const POINT_BYTES: usize = 20;
    const LINK_BYTES: usize = 40;
    let points = stream.attributes.attr1 as usize;
    let links = stream.attributes.attr2 as usize;
    let size = points as u64 * POINT_BYTES as u64 + links as u64 * LINK_BYTES as u64;
    if size != stream.bytes.len() as u64 {
        return Err(MshError::InvalidGeometry(
            "invalid Res17 point table length".into(),
        ));
    }
    let (point_bytes, link_bytes) = stream.bytes.split_at(points * POINT_BYTES);
    let points = point_bytes
        .chunks_exact(POINT_BYTES)
        .map(|point| {
            Ok(HallwayPoint {
                position: [
                    read_f32(point, 0)?,
                    read_f32(point, 4)?,
                    read_f32(point, 8)?,
                ],
                flags: read_u32(point, 12)?,
                node: NodeId(read_u32(point, 16)?),
            })
        })
        .collect::<Result<_, MshError>>()?;
    let links = link_bytes
        .chunks_exact(LINK_BYTES)
        .map(|link| Ok([read_u32(link, 0)?, read_u32(link, 4)?]))
        .collect::<Result<_, MshError>>()?;
    Ok(Hallway { points, links })
}

impl Hallway {
    pub(super) fn hero_exits(
        &self,
        node_count: usize,
    ) -> Result<Vec<(NodeId, [f32; 3])>, MshError> {
        self.points
            .iter()
            .filter(|point| point.flags & 0x1000_4000 == 0x4000)
            .map(|point| {
                if point.node.0 as usize >= node_count
                    || !point.position.iter().all(|axis| axis.is_finite())
                {
                    return Err(MshError::InvalidGeometry(
                        "invalid Res17 hero exit point".into(),
                    ));
                }
                Ok((point.node, point.position))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EntryAttributes;

    #[test]
    fn res17_retains_capture_and_exit_points_in_link_order() {
        let bytes = [
            1.0_f32.to_bits(),
            0,
            0,
            0x40,
            4, // Capture point.
            0,
            0,
            0,
            0x4000,
            2, // Hero exit.
            1,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0, // One 40-byte link.
        ]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect();
        let hallway = parse(&RawStream {
            attributes: EntryAttributes {
                attr1: 2,
                attr2: 1,
                attr3: 0,
            },
            bytes,
        })
        .unwrap();
        assert_eq!(
            hallway.points[0],
            HallwayPoint {
                position: [1.0, 0.0, 0.0],
                flags: 0x40,
                node: NodeId(4),
            }
        );
        assert_eq!(hallway.links, [[1, 0]]);
        assert_eq!(hallway.hero_exits(5).unwrap(), [(NodeId(2), [0.0; 3])]);
    }
}
