use tracing::trace;
use ferrumc_core::transform::grounded::OnGround;
use ferrumc_core::transform::position::Position;
use ferrumc_core::transform::rotation::Rotation;
use ferrumc_macros::event_handler;
use ferrumc_net::errors::NetError;
use ferrumc_net::utils::ecs_helpers::EntityExt;
use ferrumc_net::GlobalState;
use ferrumc_net::packets::packet_events::TransformEvent;

#[event_handler]
async fn handle_player_move(
    event: TransformEvent,
    state: GlobalState,
) -> Result<TransformEvent, NetError> {
    let conn_id = event.conn_id;
    if let Some(ref new_position) = event.position {
        let mut position = conn_id.get_mut::<Position>(&state)?;

        *position = Position::new(
            new_position.x,
            new_position.y,
            new_position.z,
        );

        trace!("updating player position to: {:?}", position);
    }

    if let Some(ref new_rotation) = event.rotation {
        let mut rotation = conn_id.get_mut::<Rotation>(&state)?;

        *rotation = Rotation::new(
            new_rotation.yaw,
            new_rotation.pitch,
        );

        trace!("updating player rotation to: {:?}", rotation);
    }

    if let Some(new_grounded) = event.on_ground {
        let mut on_ground = conn_id.get_mut::<OnGround>(&state)?;

        *on_ground = OnGround(new_grounded);
        
        trace!("updating player on_ground to: {:?}", on_ground);
    }

    Ok(event)
}