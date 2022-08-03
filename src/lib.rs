use std::fmt::Debug;

type Annot = Vec<String>;

pub trait Encodable {
    fn encode_to_buffer(&self, buffer: &mut Vec<u8>) -> usize;
    fn decode_from_buffer(buffer: &[u8]) -> Result<(Self, usize), &str> where Self: Sized;
}

#[derive(Debug, PartialEq)]
pub enum Node<P: Encodable> {
    Int(i32),
    String(std::string::String),
    Bytes(Vec<u8>),
    Prim(P, Vec<Node<P>>, Annot),
    Seq(Vec<Node<P>>)
}

fn write_int_be(buffer: &mut Vec<u8>, value: i32) {
    buffer.push((value >> 24 & 0xff) as u8);
    buffer.push((value >> 16 & 0xff) as u8);
    buffer.push((value >>  8 & 0xff) as u8);
    buffer.push((value       & 0xff) as u8);
}

fn read_int_be(buffer: &[u8]) -> i32 {
    ((buffer[0] as i32) << 24) |
    ((buffer[1] as i32) << 16) |
    ((buffer[2] as i32) <<  8) |
    ((buffer[3] as i32)     )
}

fn write_int_be_into_offset(buffer: &mut Vec<u8>, value: i32, offset: usize) {
    buffer[offset    ] = (value >> 24 & 0xff) as u8;
    buffer[offset + 1] = (value >> 16 & 0xff) as u8;
    buffer[offset + 2] = (value >>  8 & 0xff) as u8;
    buffer[offset + 3] = (value       & 0xff) as u8;
}

fn write_array(buffer: &mut Vec<u8>, value: &[u8]) -> usize {
    let length = value.len();
    write_int_be(buffer, length as i32);
    for byte in value {
        buffer.push(*byte);
    }
    4 + length
}

fn write_list<P: Encodable + Debug>(buffer: &mut Vec<u8>, values: &Vec<Node<P>>) -> usize {
    let size_offset = buffer.len();
    let mut size = 0;
    write_int_be(buffer, 0);
    for value in values {
        size += value.encode_to_buffer(buffer);
    }
    write_int_be_into_offset(buffer, size as i32, size_offset);
    size + 4
}

fn write_zarith(buffer: &mut Vec<u8>, value: i32) -> usize {
    // We're assuming only 32 bit integers for now, which are fairely
    // supported by WASM runtimes. Later on, we'll provide an interface
    // for plugging in arbitrary precision integer libraries.
    // TODO: How to require an interface for arbitrary size integers
    // without depending on a specific library?

    let sign = value < 0;
    let mut size = 0;
    let mut value = value.abs();

    let mut first = if value > 0x3f { value & 0x3f | 0x80 } else { value & 0x3f } as u8;

    if sign {
        first = first | 0x40;
    }

    buffer.push(first);
    size += 1;
    value = value >> 6;

    while value != 0 {
        let byte = if value > 0x7f { value & 0x7f | 0x80 } else { value & 0x7f } as u8;
        buffer.push(byte);

        size += 1;
        value = value >> 7;
    }

    size
}

fn read_zarith(buffer: &[u8]) -> (i32, usize) {
    let mut byte = buffer[0] as i32;
    let mut value = byte & 0x3f;
    let mut shift = 6;
    let mut index = 1;

    let sign = byte & 0x40 == 0x40;

    while (byte & 0x80) == 0x80 {
        byte = buffer[index] as i32;
        value = value | ((byte & 0x7f) << shift);

        index += 1;
        shift += 7;
    }

    if sign { (-value, index as usize) }
    else { (value, index) }
}

fn read_list<P: Encodable + Debug>(buffer: &[u8]) -> Result<(Vec<Node<P>>, usize), &str> {
    let size = read_int_be(buffer) as usize;
    let mut items = Vec::new();
    let mut offset = 4;

    while offset < (size + 4) {
        let (item, size) = Node::<P>::from_offset(buffer, offset)?;
        offset += size;
        items.push(item);
    }

    Ok((items, size + 4))
}

fn read_vec(buffer: &[u8]) -> Result<(Vec<u8>, usize), &str> {
    let size = read_int_be(buffer) as usize;
    let value = (&buffer[4..size + 4]).to_vec();

    Ok((value, size + 4))
}

fn read_annotation(buffer: &[u8]) -> Result<(Vec<String>, usize), &str> {
    let (vec, size) = read_vec(buffer)?;
    let annot = String::from_utf8(vec).expect("Only UTF-8 allowed");

    Ok((annot.split(" ").map(String::from).collect(), size))
}

fn encode_annotation(buffer: &mut Vec<u8>, annot: &Vec<String>) -> usize {
    // TODO: Different semantics
    let annot = annot.join(" ");
    write_array(buffer, &annot.as_bytes())
}

fn encode_primitive<P: Encodable + Debug>(buffer: &mut Vec<u8>, prim: &P, args: &Vec<Node<P>>, annot: &Vec<String>) -> usize {
    match (&args[..], &annot[..]) {
        ([], []) => {
            buffer.push(3);
            prim.encode_to_buffer(buffer) + 1
        },
        ([], _) => {
            buffer.push(4);
            prim.encode_to_buffer(buffer)
            + encode_annotation(buffer, annot)
            + 1
        },
        ([arg1], []) => {
            buffer.push(5);
            prim.encode_to_buffer(buffer)
            + arg1.encode_to_buffer(buffer)
            + 1
        },
        ([arg1], _) => {
            buffer.push(6);
            prim.encode_to_buffer(buffer)
            + arg1.encode_to_buffer(buffer)
            + encode_annotation(buffer, annot)
            + 1
        },
        ([arg1, arg2], []) => {
            buffer.push(7);
            prim.encode_to_buffer(buffer)
            + arg1.encode_to_buffer(buffer)
            + arg2.encode_to_buffer(buffer)
            + 1
        },
        ([arg1, arg2], _) => {
            buffer.push(8);
            prim.encode_to_buffer(buffer)
            + arg1.encode_to_buffer(buffer)
            + arg2.encode_to_buffer(buffer)
            + encode_annotation(buffer, annot)
            + 1
        }
        (_, _) => {
            buffer.push(9);
            prim.encode_to_buffer(buffer)
            + write_list(buffer, args)
            + encode_annotation(buffer, annot)
            + 1
        }
    }
}

impl<P: Encodable + Debug> Node<P> {
    fn encode_to_buffer(self: &Node<P>, buffer: &mut Vec<u8>) -> usize {
        match self {
            Node::Int(v) => {
                buffer.push(0);
                write_zarith(buffer, *v) + 1
            },
            Node::String(v) => {
                buffer.push(1);
                write_array(buffer, v.as_bytes())
            },
            Node::Bytes(v) => {
                buffer.push(10);
                write_array(buffer, v)
            },
            Node::Seq(v) => {
                buffer.push(2);
                write_list(buffer, v) + 1
            },
            Node::Prim(prim, args, annot) => {
                encode_primitive(buffer, prim, args, annot)
            },
        }
    }

    pub fn encode(self: Node<P>) -> Vec<u8> {
        let mut buffer = Vec::new();
        self.encode_to_buffer(&mut buffer);
        buffer
    }

    fn from_offset(buffer: &[u8], offset: usize) -> Result<(Node<P>, usize), &str> {
        match buffer[offset] {
            0 => {
                let (value, size) = read_zarith(&buffer[offset + 1..]);
                Ok((Node::Int(value), size + 1))
            },
            1 => {
                let (value, size) = read_vec(&buffer[offset + 1..])?;
                let string = String::from_utf8(value).expect("Only UTF-8 allowed");
                Ok((Node::String(string), size + 1))
            },
            2 => {
                let (items, size) = read_list(&buffer[offset + 1..])?;
                Ok((Node::Seq(items), size + 1))
            },
            3 => {
                let (prim, size) = P::decode_from_buffer(&buffer[offset + 1..])?;
                Ok((Node::Prim(prim, vec![], vec![]), size + 1))
            },
            4 => {
                let (prim, prim_size) = P::decode_from_buffer(&buffer[offset + 1..])?;
                let (annot, annot_size) = read_annotation(&buffer[offset + prim_size + 1..])?;
                Ok((Node::Prim(prim, vec![], annot), prim_size + annot_size + 1))
            },
            5 => {
                let (prim, prim_size) = P::decode_from_buffer(&buffer[offset + 1..])?;
                let (arg, arg_size) = Node::from_offset(buffer, offset + prim_size + 1)?;
                Ok((Node::Prim(prim, vec![arg], vec![]), prim_size + arg_size + 1))
            },
            6 => {
                let (prim, prim_size) = P::decode_from_buffer(&buffer[offset + 1..])?;
                let (arg, arg_size) = Node::from_offset(buffer, offset + prim_size + 1)?;
                let (annot, annot_size) = read_annotation(&buffer[offset + prim_size + arg_size + 1..])?;
                Ok((Node::Prim(prim, vec![arg], annot), prim_size + arg_size + annot_size + 1))
            },
            7 => {
                let (prim, prim_size) = P::decode_from_buffer(&buffer[offset + 1..])?;
                let (arg1, arg1_size) = Node::from_offset(buffer, offset + prim_size + 1)?;
                let (arg2, arg2_size) = Node::from_offset(buffer, offset + prim_size + arg1_size + 1)?;
                Ok((Node::Prim(prim, vec![arg1, arg2], vec![]), prim_size + arg1_size + arg2_size + 1))
            },
            8 => {
                let (prim, prim_size) = P::decode_from_buffer(&buffer[offset + 1..])?;
                let (arg1, arg1_size) = Node::from_offset(buffer, offset + prim_size + 1)?;
                let (arg2, arg2_size) = Node::from_offset(buffer, offset + prim_size + arg1_size + 1)?;
                let (annot, annot_size) = read_annotation(&buffer[offset + prim_size + arg1_size + arg2_size + 1..])?;
                Ok((Node::Prim(prim, vec![arg1, arg2], annot), prim_size + arg1_size + arg2_size + annot_size + 1))
            },
            9 => {
                let (prim, prim_size) = P::decode_from_buffer(&buffer[offset + 1..])?;
                let (args, args_size) = read_list(&buffer[offset + prim_size + 1..])?;
                let (annot, annot_size) = read_annotation(&buffer[offset + prim_size + args_size + 1..])?;
                Ok((Node::Prim(prim, args, annot), prim_size + args_size + annot_size))
            },
            10 => {
                let (value, size) = read_vec(&buffer[offset + 1..])?;
                Ok((Node::Bytes(value), size + 1))
            }
            _ => Err("Invalid value")
        }
    }

    pub fn from(buffer: &[u8]) -> Result<Node<P>, &str> {
        let (value, _) = Node::from_offset(buffer, 0)?;
        Ok(value)
    }

}

pub mod michelson_v1_primitives;
use michelson_v1_primitives::{*};

impl Encodable for Primitive {
    fn encode_to_buffer(&self, buffer: &mut Vec<u8>) -> usize {
        buffer.push(self.to_int_enum());
        1
    }

    fn decode_from_buffer(buffer: &[u8]) -> Result<(Self, usize), &str> where Self: Sized {
        Primitive::from_int_enum(buffer[0])
            .map(|value| (value, 1))
            .ok_or("Invalid primitive value")
    }
}

#[cfg(test)]
mod tests {
    use crate::{*};

    #[derive(Debug, PartialEq)]
    struct DummyPrimitive;
    impl Encodable for DummyPrimitive {
        fn encode_to_buffer(&self, buffer: &mut Vec<u8>) -> usize {
            buffer.push(0);
            1
        }

        fn decode_from_buffer(buffer: &[u8]) -> Result<(Self, usize), &str> where Self: Sized {
            if buffer[0] != 0 {
                return Err("Invalid DummyPrimitive");
            }

            Ok((DummyPrimitive, 1))
        }
    }

    #[test]
    fn integers() {
        assert_eq!(Node::Int::<DummyPrimitive>(0).encode(), b"\x00\x00");
        assert_eq!(Node::Int::<DummyPrimitive>(0x1337).encode(), b"\x00\xb7\x4c");
        assert_eq!(Node::Int::<DummyPrimitive>(-0x1337).encode(), b"\x00\xf7\x4c");
        assert_eq!(Node::Int::<DummyPrimitive>(1996).encode(), b"\x00\x8c\x1f");
        assert_eq!(Node::Int::<DummyPrimitive>(-1996).encode(), b"\x00\xcc\x1f");
        assert_eq!(Node::Int::<DummyPrimitive>(0x616263).encode(), b"\x00\xa3\x89\x8b\x06");
        assert_eq!(Node::Int::<DummyPrimitive>(-0x616263).encode(), b"\x00\xe3\x89\x8b\x06");

        assert_eq!(Node::<DummyPrimitive>::from(b"\x00\x00").unwrap(), Node::Int(0));
        assert_eq!(Node::<DummyPrimitive>::from(b"\x00\xb7\x4c").unwrap(), Node::Int(0x1337));
        assert_eq!(Node::<DummyPrimitive>::from(b"\x00\xf7\x4c").unwrap(), Node::Int(-0x1337));
        assert_eq!(Node::<DummyPrimitive>::from(b"\x00\x8c\x1f").unwrap(), Node::Int(1996));
        assert_eq!(Node::<DummyPrimitive>::from(b"\x00\xcc\x1f").unwrap(), Node::Int(-1996));
        assert_eq!(Node::<DummyPrimitive>::from(b"\x00\xa3\x89\x8b\x06").unwrap(), Node::Int(0x616263));
        assert_eq!(Node::<DummyPrimitive>::from(b"\x00\xe3\x89\x8b\x06").unwrap(), Node::Int(-0x616263));
    }

    #[test]
    fn strings() {
        assert_eq!(
            Node::String::<DummyPrimitive>(String::from("Hello world")).encode(),
            b"\x01\x00\x00\x00\x0bHello world"
        );
        assert_eq!(
            Node::String::<DummyPrimitive>(String::from("")).encode(),
            b"\x01\x00\x00\x00\x00"
        );

        assert_eq!(
            Node::<DummyPrimitive>::from(b"\x01\x00\x00\x00\x0bHello world").unwrap(),
            Node::String::<DummyPrimitive>(String::from("Hello world")),
        );
        assert_eq!(
            Node::<DummyPrimitive>::from(b"\x01\x00\x00\x00\x00").unwrap(),
            Node::String::<DummyPrimitive>(String::from(""))
        );
    }

    #[test]
    fn bytes_() {
        assert_eq!(
            Node::Bytes::<DummyPrimitive>("Hello world".as_bytes().to_vec()).encode(),
            b"\x0a\x00\x00\x00\x0bHello world"
        );

        assert_eq!(
            Node::<DummyPrimitive>::from(b"\x0a\x00\x00\x00\x0bHello world").unwrap(),
            Node::Bytes::<DummyPrimitive>("Hello world".as_bytes().to_vec()),
        );
    }

    #[test]
    fn seqs() {
        assert_eq!(
            Node::Seq::<DummyPrimitive>(
                vec![Node::Int(1), Node::Int(2)]
            ).encode(),
            b"\x02\x00\x00\x00\x04\x00\x01\x00\x02"
        );

        assert_eq!(
            Node::from(b"\x02\x00\x00\x00\x04\x00\x01\x00\x02").unwrap(),
            Node::Seq::<DummyPrimitive>(
                vec![Node::Int(1), Node::Int(2)]
            )
        );
    }

    #[test]
    fn primitive_no_args_no_annot() {
        assert_eq!(
            Node::Prim(
                DummyPrimitive,
                vec![],
                vec![]
            ).encode(),
            b"\x03\x00"
        );

        assert_eq!(
            Node::from(b"\x03\x00").unwrap(),
            Node::Prim(
                DummyPrimitive,
                vec![],
                vec![]
            )
        );
    }

    #[test]
    fn primitive_no_args_some_annot() {
        assert_eq!(
            Node::Prim(
                DummyPrimitive,
                vec![],
                vec![String::from("%annot1")],
            ).encode(),
            b"\x04\x00\x00\x00\x00\x07%annot1"
        );
        assert_eq!(
            Node::Prim(
                DummyPrimitive,
                vec![],
                vec![String::from("%annot1"), String::from("%annot2")],
            ).encode(),
            b"\x04\x00\x00\x00\x00\x0f%annot1 %annot2"
        );
        assert_eq!(
            Node::Prim(
                DummyPrimitive,
                vec![],
                vec![String::from("%annot1"), String::from("%annot2"), String::from("%annot3")],
            ).encode(),
            b"\x04\x00\x00\x00\x00\x17%annot1 %annot2 %annot3"
        );

        assert_eq!(
            Node::from(b"\x04\x00\x00\x00\x00\x07%annot1").unwrap(),
            Node::Prim(
                DummyPrimitive,
                vec![],
                vec![String::from("%annot1")],
            )
        );
        assert_eq!(
            Node::from(b"\x04\x00\x00\x00\x00\x0f%annot1 %annot2").unwrap(),
            Node::Prim(
                DummyPrimitive,
                vec![],
                vec![String::from("%annot1"), String::from("%annot2")],
            )
        );
        assert_eq!(
            Node::from(b"\x04\x00\x00\x00\x00\x17%annot1 %annot2 %annot3").unwrap(),
            Node::Prim(
                DummyPrimitive,
                vec![],
                vec![String::from("%annot1"), String::from("%annot2"), String::from("%annot3")],
            )
        );
    }

    #[test]
    fn primitive_one_args_no_annot() {
        assert_eq!(
            Node::Prim(
                DummyPrimitive,
                vec![Node::Int(42)],
                vec![],
            ).encode(),
            b"\x05\x00\x00\x2a"
        );
        assert_eq!(
            Node::Prim(
                DummyPrimitive,
                vec![Node::String(String::from("Hello world"))],
                vec![],
            ).encode(),
            b"\x05\x00\x01\x00\x00\x00\x0bHello world"
        );

        assert_eq!(
            Node::from(b"\x05\x00\x00\x2a").unwrap(),
            Node::Prim(
                DummyPrimitive,
                vec![Node::Int(42)],
                vec![],
            )
        );
        assert_eq!(
            Node::from(b"\x05\x00\x01\x00\x00\x00\x0bHello world").unwrap(),
            Node::Prim(
                DummyPrimitive,
                vec![Node::String(String::from("Hello world"))],
                vec![],
            )
        );
    }

    #[test]
    fn primitive_one_arg_some_annots() {
        assert_eq!(
            Node::Prim(
                DummyPrimitive,
                vec![Node::Int(42)],
                vec![String::from("%annot1"), String::from("%annot2")],
            ).encode(),
            b"\x06\x00\x00\x2a\x00\x00\x00\x0f%annot1 %annot2"
        );
        assert_eq!(
            Node::Prim(
                DummyPrimitive,
                vec![Node::Int(42)],
                vec![String::from("%annot1")],
            ).encode(),
            b"\x06\x00\x00\x2a\x00\x00\x00\x07%annot1"
        );

        assert_eq!(
            Node::from(b"\x06\x00\x00\x2a\x00\x00\x00\x0f%annot1 %annot2").unwrap(),
            Node::Prim(
                DummyPrimitive,
                vec![Node::Int(42)],
                vec![String::from("%annot1"), String::from("%annot2")],
            )
        );
        assert_eq!(
            Node::from(b"\x06\x00\x00\x2a\x00\x00\x00\x07%annot1").unwrap(),
            Node::Prim(
                DummyPrimitive,
                vec![Node::Int(42)],
                vec![String::from("%annot1")],
            )
        );
    }

    #[test]
    fn primitive_two_args_no_annots() {
        assert_eq!(
            Node::Prim(
                DummyPrimitive,
                vec![Node::Int(42), Node::String(String::from("Hello world"))],
                vec![],
            ).encode(),
            b"\x07\x00\x00\x2a\x01\x00\x00\x00\x0bHello world"
        );

        assert_eq!(
            Node::from(b"\x07\x00\x00\x2a\x01\x00\x00\x00\x0bHello world").unwrap(),
            Node::Prim(
                DummyPrimitive,
                vec![Node::Int(42), Node::String(String::from("Hello world"))],
                vec![],
            )
        );
    }

    #[test]
    fn primitive_two_args_some_annots() {
        assert_eq!(
            Node::Prim(
                DummyPrimitive,
                vec![Node::Int(42), Node::String(String::from("Hello world"))],
                vec![String::from("%annot1"), String::from("%annot2")],
            ).encode(),
            b"\x08\x00\x00\x2a\x01\x00\x00\x00\x0bHello world\x00\x00\x00\x0f%annot1 %annot2"
        );

        assert_eq!(
            Node::from(b"\x08\x00\x00\x2a\x01\x00\x00\x00\x0bHello world\x00\x00\x00\x0f%annot1 %annot2").unwrap(),
            Node::Prim(
                DummyPrimitive,
                vec![Node::Int(42), Node::String(String::from("Hello world"))],
                vec![String::from("%annot1"), String::from("%annot2")],
            )
        );
    }

    #[test]
    fn primitive_application() {
        assert_eq!(
            Node::Prim(
                DummyPrimitive,
                vec![Node::Int(42), Node::Int(43), Node::Int(44)],
                vec![]
            ).encode(),
            b"\x09\x00\x00\x00\x00\x06\x00\x2a\x00\x2b\x00\x2c\x00\x00\x00\x00"
        );
        assert_eq!(
            Node::Prim(
                DummyPrimitive,
                vec![Node::Int(42), Node::Int(43), Node::Int(44)],
                vec![String::from("%annot1"), String::from("%annot2")]
            ).encode(),
            b"\x09\x00\x00\x00\x00\x06\x00\x2a\x00\x2b\x00\x2c\x00\x00\x00\x0f%annot1 %annot2"
        );

        // assert_eq!(
        //     Node::from(b"\x09\x00\x00\x00\x00\x06\x00\x2a\x00\x2b\x00\x2c\x00\x00\x00\x00").unwrap(),
        //     Node::Prim(
        //         DummyPrimitive,
        //         vec![Node::Int(42), Node::Int(43), Node::Int(44)],
        //         vec![]
        //     )
        // );
        assert_eq!(
            Node::from(b"\x09\x00\x00\x00\x00\x06\x00\x2a\x00\x2b\x00\x2c\x00\x00\x00\x0f%annot1 %annot2").unwrap(),
            Node::Prim(
                DummyPrimitive,
                vec![Node::Int(42), Node::Int(43), Node::Int(44)],
                vec![String::from("%annot1"), String::from("%annot2")]
            )
        );
    }

    #[test]
    fn michelson_v1_primitives() {
        use michelson_v1_primitives::Primitive::{D_Pair, I_PUSH, I_ADD, T_nat};

        assert_eq!(
            Node::Prim(
                D_Pair,
                vec![
                    Node::String(String::from("KT1BuEZtb68c1Q4yjtckcNjGELqWt56Xyesc")),
                    Node::Bytes(b"deadbeef".to_vec())
                ],
                vec![]
            ).encode(),
            b"\x07\x07\x01\x00\x00\x00\x24KT1BuEZtb68c1Q4yjtckcNjGELqWt56Xyesc\x0a\x00\x00\x00\x08deadbeef"
        );
        assert_eq!(
            Node::Seq(vec![
                Node::Prim(
                    I_PUSH,
                    vec![
                        Node::Prim(T_nat, vec![], vec![]),
                        Node::Int(1),
                    ],
                    vec![String::from("%one")]
                ),
                Node::Prim(
                    I_PUSH,
                    vec![
                        Node::Prim(T_nat, vec![], vec![]),
                        Node::Int(2),
                    ],
                    vec![String::from("%two")]
                ),
                Node::Prim(I_ADD, vec![], vec![])
            ]).encode(),
            b"\x02\x00\x00\x00\x1e\x08\x43\x03\x62\x00\x01\x00\x00\x00\x04%one\x08\x43\x03\x62\x00\x02\x00\x00\x00\x04%two\x03\x12"
        );


        assert_eq!(
            Node::from(b"\x07\x07\x01\x00\x00\x00\x24KT1BuEZtb68c1Q4yjtckcNjGELqWt56Xyesc\x0a\x00\x00\x00\x08deadbeef").unwrap(),
            Node::Prim(
                D_Pair,
                vec![
                    Node::String(String::from("KT1BuEZtb68c1Q4yjtckcNjGELqWt56Xyesc")),
                    Node::Bytes(b"deadbeef".to_vec())
                ],
                vec![]
            ),
        );
        assert_eq!(
            Node::from(b"\x02\x00\x00\x00\x1e\x08\x43\x03\x62\x00\x01\x00\x00\x00\x04%one\x08\x43\x03\x62\x00\x02\x00\x00\x00\x04%two\x03\x12").unwrap(),
            Node::Seq(vec![
                Node::Prim(
                    I_PUSH,
                    vec![
                        Node::Prim(T_nat, vec![], vec![]),
                        Node::Int(1),
                    ],
                    vec![String::from("%one")]
                ),
                Node::Prim(
                    I_PUSH,
                    vec![
                        Node::Prim(T_nat, vec![], vec![]),
                        Node::Int(2),
                    ],
                    vec![String::from("%two")]
                ),
                Node::Prim(I_ADD, vec![], vec![])
            ])
        );

    }
}
