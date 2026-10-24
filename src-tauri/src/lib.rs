/**
 * @Author: Danson zheng
 * @Date: 2026-10-23
 * @Description: token-tracker 后端入口，导出数据模型、聚合逻辑与桌面壳启动函数。
 */

pub mod aggregator;
pub mod collector;
mod desktop;
mod detection;
pub mod live;
pub mod models;
mod power;
pub mod tools;

pub use desktop::run;
