use arrow::array::{
    Array, Float64Array, GenericListArray, Int64Array, ListArray, PrimitiveArray, UInt32Array,
    UInt64Array,
};
use arrow::datatypes::DataType;
use clap::ArgMatches;
use std::error::Error;
use std::path::PathBuf;

use super::read_parquet_footer;

fn compare_column<T: arrow::array::ArrowPrimitiveType>(
    column: &str,
    x: Option<&PrimitiveArray<T>>,
    y: Option<&PrimitiveArray<T>>,
    row_idx: Option<usize>,
) -> Result<(), Box<dyn Error>>
where
    <T as arrow::array::ArrowPrimitiveType>::Native: std::fmt::Display,
{
    let v1 = x.ok_or("invalid downcast for column")?;
    let v2 = y.ok_or("invalid downcast for column")?;

    v1.into_iter().zip(v2).enumerate().for_each(|(i, (a, b))| {
        if a != b {
            let row = row_idx.unwrap_or(i);
            let x_str = x
                .map(|_| v1.value(i).to_string())
                .unwrap_or("None".to_string());
            let y_str = y
                .map(|_| v2.value(i).to_string())
                .unwrap_or("None".to_string());
            eprintln!(
                "Values mismatch for {column}; row {}: {} vs {}",
                row, x_str, y_str
            );
        }
    });

    Ok(())
}

fn compare_listarray_column(
    column: &str,
    v1: &GenericListArray<i32>,
    v2: &GenericListArray<i32>,
    data_type: &DataType,
) -> Result<(), Box<dyn Error>> {
    for (idx, (x, y)) in v1.iter().zip(v2.iter()).enumerate() {
        if (x.is_none() && y.is_some()) || (x.is_some() && y.is_none()) {
            eprintln!("Values mismatch for {column}; row {idx}, only one is None");
            continue;
        }

        if let (Some(a), Some(b)) = (x, y) {
            match data_type {
                DataType::UInt32 => {
                    let l = a.as_any().downcast_ref::<UInt32Array>();
                    let r = b.as_any().downcast_ref::<UInt32Array>();
                    compare_column(column, l, r, Some(idx))?;
                }
                DataType::UInt64 => {
                    let l = a.as_any().downcast_ref::<UInt64Array>();
                    let r = b.as_any().downcast_ref::<UInt64Array>();
                    compare_column(column, l, r, Some(idx))?;
                }
                _ => {
                    eprintln!("Unexpected inner data type for {column}: {data_type}");
                }
            };
        }
    }

    Ok(())
}

pub(super) fn run(args: &ArgMatches) -> Result<(), Box<dyn Error>> {
    let left = args.get_one::<PathBuf>("left").unwrap();
    let right = args.get_one::<PathBuf>("right").unwrap();

    let (_, s1, r1) = read_parquet_footer(left)?;
    let (_, s2, mut r2) = read_parquet_footer(right)?;

    // Iterate across record batch from left
    for x in r1 {
        let x = x?;
        let Some(y) = r2.next() else {
            return Err(format!("{} iterator completed earlier", right.display()).into());
        };
        let y = y?;

        // Map every column from left to the corresponding column on right
        for (idx, l) in x.columns().iter().enumerate() {
            let column = s1.field(idx).name();
            let Some(r) = y.column_by_name(column) else {
                eprintln!("{column} missing in {}", right.display());
                continue;
            };

            let (d1, d2) = (l.data_type(), r.data_type());
            if d1 != d2 {
                return Err(format!("Data type mismatch for {}: {} vs {}", column, d1, d2).into());
            }

            let (l_any, r_any) = (l.as_any(), r.as_any());
            match d1 {
                DataType::UInt32 => {
                    let v1 = l_any.downcast_ref::<UInt32Array>();
                    let v2 = r_any.downcast_ref::<UInt32Array>();
                    compare_column(column, v1, v2, None)?;
                }
                DataType::UInt64 => {
                    let v1 = l_any.downcast_ref::<UInt64Array>();
                    let v2 = r_any.downcast_ref::<UInt64Array>();
                    compare_column(column, v1, v2, None)?;
                }
                DataType::Int64 => {
                    let v1 = l_any.downcast_ref::<Int64Array>();
                    let v2 = r_any.downcast_ref::<Int64Array>();
                    compare_column(column, v1, v2, None)?;
                }
                DataType::Float64 => {
                    let v1 = l_any.downcast_ref::<Float64Array>();
                    let v2 = r_any.downcast_ref::<Float64Array>();
                    compare_column(column, v1, v2, None)?;
                }
                DataType::List(field_type) => {
                    let v1 = l_any
                        .downcast_ref::<ListArray>()
                        .ok_or("not a list array")?;
                    let v2 = r_any
                        .downcast_ref::<ListArray>()
                        .ok_or("not a list array")?;
                    compare_listarray_column(column, v1, v2, field_type.data_type())?;
                }
                _ => {
                    eprintln!("Unexpected type for {column}: {}", d1);
                }
            };
        }

        // Display columns only in the right
        for (idx, _) in y.columns().iter().enumerate() {
            let column = s2.field(idx).name();
            if x.column_by_name(column).is_none() {
                eprintln!("{column} missing in {}", left.display());
            }
        }
    }

    // Fill in data in case right has more record batches
    if r2.next().is_some() {
        return Err(format!("{} iterator completed earlier", left.display()).into());
    }

    Ok(())
}
