use super::{MAX_MIX_BUFFER_SIZE, world::BusWorld};

/// バス処理システム。
///
/// `BusWorld` の mix_buffer に対して gain・mute 処理を行い、
/// 最終出力を output_buffer に書き出す。
pub struct BusSystem;

impl BusSystem {
    /// バス処理を行い、最終出力を `output_buffer` に書き出す。
    ///
    /// `process_order` 順（リーフ→ルート）に:
    /// 1. mute されていれば当該バスのスライスをゼロ埋め、そうでなければ gain を乗算。
    /// 2. マスターバス以外は親バスの mix_buffer に加算。
    /// 3. マスターバスの mix_buffer を `output_buffer` にコピー。
    pub fn update(
        world: &mut BusWorld,
        output_buffer: &mut [f32],
        _device_channels: usize,
        sample_count: usize,
    ) {
        let sample_count = sample_count.min(MAX_MIX_BUFFER_SIZE);
        let master_dense = world.resolve(world.master_entity()).unwrap_or(0);

        // process_order をコピーして world.mix_buffer の可変借用と干渉しないようにする。
        let order: Vec<u32> = world.process_order.clone();

        for &d in &order {
            let d = d as usize;
            let start = d * MAX_MIX_BUFFER_SIZE;

            if world.muted[d] {
                world.mix_buffer[start..start + sample_count].fill(0.0);
            } else {
                let g = world.gain[d];
                if g != 1.0 {
                    for s in &mut world.mix_buffer[start..start + sample_count] {
                        *s *= g;
                    }
                }
            }

            if d != master_dense {
                let parent = world.output_bus_dense[d] as usize;
                let parent_start = parent * MAX_MIX_BUFFER_SIZE;
                debug_assert_ne!(d, parent, "バスが自己参照しています");
                // SAFETY: d != parent（木構造なので自己参照なし）。
                // d と parent は異なるバスのスライスを指すため、重複しない。
                unsafe {
                    let src_ptr = world.mix_buffer.as_ptr().add(start);
                    let dst_ptr = world.mix_buffer.as_mut_ptr().add(parent_start);
                    for i in 0..sample_count {
                        *dst_ptr.add(i) += *src_ptr.add(i);
                    }
                }
            }
        }

        let master_start = master_dense * MAX_MIX_BUFFER_SIZE;
        let copy_len = sample_count.min(output_buffer.len());
        output_buffer[..copy_len]
            .copy_from_slice(&world.mix_buffer[master_start..master_start + copy_len]);
    }
}
