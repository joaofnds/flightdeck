use persistence::Window;
use persistence::schema::Table;

use super::query_support::Tables;

pub fn append(tables: &Tables, table: Table, window: &Window) {
    let frame = persistence::frame(table, window).unwrap();

    tables.append(table.name(), &frame);
}
