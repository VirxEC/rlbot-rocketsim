use rocketsim::CarBodyConfig;

/// Maps Rocket League body product IDs to RocketSim hitbox families.
///
/// Adapted from VirxEC/replay-to-rocketsim `src/body.rs` at commit
/// 320c5f0b13c43fe3a3b93df358b8669ff611975d (MIT license).
#[must_use]
pub const fn car_body_config_for_product_id(product_id: u32) -> Option<CarBodyConfig> {
    match product_id {
        21 | 23 | 25 | 26 | 27 | 402 | 404 | 523 | 607 | 625 | 723 | 1172 | 1295 | 1300 | 1475
        | 1478 | 1533 | 1568 | 1623 | 2313 | 2665 | 2853 | 2919 | 2949 | 4284 | 4318 | 4319
        | 4320 | 4782 | 4906 | 5020 | 5039 | 5188 | 5361 | 5547 | 5713 | 5837 | 5951 | 6939
        | 7947 | 7948 | 8383 | 8806 | 8807 | 10896 | 10897 | 10900 | 10901 | 11603 => {
            Some(CarBodyConfig::OCTANE)
        }
        29 | 403 | 597 | 600 | 1018 | 1171 | 1286 | 1675 | 1689 | 1883 | 2070 | 2268 | 2298
        | 2666 | 2950 | 2951 | 3155 | 3156 | 3157 | 3265 | 3426 | 3875 | 3879 | 3880 | 4014
        | 4155 | 4367 | 4472 | 4473 | 4745 | 4770 | 4781 | 4861 | 4864 | 5709 | 5773 | 5823
        | 5858 | 5964 | 5979 | 6122 | 6244 | 6247 | 6260 | 6836 | 7211 | 7337 | 7338 | 7341
        | 7343 | 7415 | 7512 | 7532 | 7593 | 7772 | 8454 | 9053 | 9088 | 9089 | 9140 | 9388
        | 9894 | 10440 | 10441 | 10694 | 11016 | 11095 | 11315 | 11336 => {
            Some(CarBodyConfig::DOMINUS)
        }
        22 | 1416 | 1894 | 1932 | 3031 | 3311 | 6243 | 6489 | 7651 | 7696 | 7890 | 7901 | 8006
        | 8360 | 8361 | 8565 | 8566 | 8669 | 9357 | 10697 | 10698 | 10817 | 10822 | 11038
        | 11394 | 11505 | 11800 => Some(CarBodyConfig::BREAKOUT),
        30 | 4780 | 7336 | 7477 | 7815 | 7979 | 10689 | 11098 | 11314 => Some(CarBodyConfig::MERC),
        24 | 803 | 1603 | 1691 | 1919 | 3594 | 3614 | 3622 | 4268 | 5265 | 7052 | 8524 => {
            Some(CarBodyConfig::PLANK)
        }
        28 | 31 | 1159 | 1317 | 1624 | 1856 | 2269 | 3451 | 3582 | 3702 | 5470 | 5488 | 5879
        | 7012 | 9084 | 9085 | 9427 | 10044 | 10805 | 11138 | 11141 | 11379 => {
            Some(CarBodyConfig::HYBRID)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_representative_product_ids() {
        assert_eq!(
            car_body_config_for_product_id(23),
            Some(CarBodyConfig::OCTANE)
        );
        assert_eq!(
            car_body_config_for_product_id(29),
            Some(CarBodyConfig::DOMINUS)
        );
        assert_eq!(
            car_body_config_for_product_id(22),
            Some(CarBodyConfig::BREAKOUT)
        );
        assert_eq!(
            car_body_config_for_product_id(30),
            Some(CarBodyConfig::MERC)
        );
        assert_eq!(
            car_body_config_for_product_id(24),
            Some(CarBodyConfig::PLANK)
        );
        assert_eq!(
            car_body_config_for_product_id(28),
            Some(CarBodyConfig::HYBRID)
        );
        assert_eq!(car_body_config_for_product_id(u32::MAX), None);
    }
}
