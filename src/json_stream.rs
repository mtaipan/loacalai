//! Strict JSON token/range reader. Strings are decoded into bounded UTF-8 packets.
//! No Message, array, or complete string needs to be constructed while scanning.
use anyhow::{Context,Result};
use std::{fs::File,io::{BufRead,BufReader,Read,Seek,SeekFrom},path::Path};
use tokio_util::sync::CancellationToken;
#[derive(Clone,Copy,Debug)]pub struct Span{pub start:u64,pub end:u64}
pub struct Reader{input:BufReader<File>,pub position:u64,cancel:CancellationToken,steps:u64}
impl Reader{
    pub fn open(path:&Path,cancel:&CancellationToken)->Result<Self>{Ok(Self{input:BufReader::with_capacity(65536,File::open(path)?),position:0,cancel:cancel.clone(),steps:0})}
    pub fn seek(&mut self,offset:u64)->Result<()>{self.input.seek(SeekFrom::Start(offset))?;self.position=offset;Ok(())}
    fn peek(&mut self)->Result<Option<u8>>{Ok(self.input.fill_buf()?.first().copied())}
    fn byte(&mut self)->Result<u8>{self.steps+=1;if self.steps%4096==0{anyhow::ensure!(!self.cancel.is_cancelled(),"Импорт JSON остановлен");}let mut byte=[0];self.input.read_exact(&mut byte).context("Незавершённый JSON")?;self.position+=1;Ok(byte[0])}
    fn expect(&mut self,byte:u8)->Result<()>{anyhow::ensure!(self.byte()?==byte,"Неверный JSON около байта {}",self.position);Ok(())}
    pub fn whitespace(&mut self)->Result<()>{while self.peek()?.is_some_and(|b|matches!(b,b' '|b'\n'|b'\r'|b'\t')){self.byte()?;}Ok(())}
    fn hex4(&mut self)->Result<u32>{let mut number=0;for _ in 0..4{let b=self.byte()?;number=number*16+match b{b'0'..=b'9'=>(b-b'0') as u32,b'a'..=b'f'=>(b-b'a'+10) as u32,b'A'..=b'F'=>(b-b'A'+10) as u32,_=>anyhow::bail!("Неверный JSON unicode escape")};}Ok(number)}
    pub fn string(&mut self,mut sink:impl FnMut(&str)->Result<()>)->Result<()> {
        self.expect(b'"')?;let mut packet=String::with_capacity(16388);
        loop{let first=self.byte()?;let ch=match first{
            b'"'=>break,b'\\'=>match self.byte()?{
                b'"'=>'"',b'\\'=>'\\',b'/'=>'/',b'b'=>'\u{8}',b'f'=>'\u{c}',b'n'=>'\n',b'r'=>'\r',b't'=>'\t',
                b'u'=>{let high=self.hex4()?;let code=if (0xd800..=0xdbff).contains(&high){self.expect(b'\\')?;self.expect(b'u')?;let low=self.hex4()?;anyhow::ensure!((0xdc00..=0xdfff).contains(&low),"Неверная JSON surrogate pair");0x10000+((high-0xd800)<<10)+(low-0xdc00)}else{high};char::from_u32(code).context("Неверный JSON unicode scalar")?},_=>anyhow::bail!("Неверный JSON escape")},
            0..=31=>anyhow::bail!("Неэкранированный control в JSON строке"),
            32..=127=>first as char,
            _=>{let n=match first{0xc2..=0xdf=>2,0xe0..=0xef=>3,0xf0..=0xf4=>4,_=>anyhow::bail!("Некорректный UTF-8 JSON")};let mut bytes=[0;4];bytes[0]=first;for byte in &mut bytes[1..n]{*byte=self.byte()?;}std::str::from_utf8(&bytes[..n])?.chars().next().unwrap()}
        };packet.push(ch);if packet.len()>=16384{sink(&packet)?;packet.clear();}}
        if !packet.is_empty(){sink(&packet)?;}Ok(())
    }
    fn key(&mut self)->Result<String>{let mut key=String::new();let mut large=false;self.string(|part|{if key.len()+part.len()<=4096&&!large{key.push_str(part);}else{large=true;}Ok(())})?;if large{Ok(String::new())}else{Ok(key)}}
    fn scalar(&mut self)->Result<()>{
        match self.peek()?.context("Незавершённый JSON")?{
            b't'=>{for b in b"true"{self.expect(*b)?;}},b'f'=>{for b in b"false"{self.expect(*b)?;}},b'n'=>{for b in b"null"{self.expect(*b)?;}},
            b'-'|b'0'..=b'9'=>{if self.peek()?==Some(b'-'){self.byte()?;}match self.peek()?{Some(b'0')=>{self.byte()?;},Some(b'1'..=b'9')=>{self.byte()?;while self.peek()?.is_some_and(|b|b.is_ascii_digit()){self.byte()?;}},_=>anyhow::bail!("Неверное JSON число")};if self.peek()?==Some(b'.'){self.byte()?;anyhow::ensure!(self.peek()?.is_some_and(|b|b.is_ascii_digit()),"Неверная дробь");while self.peek()?.is_some_and(|b|b.is_ascii_digit()){self.byte()?;}}if self.peek()?.is_some_and(|b|b==b'e'||b==b'E'){self.byte()?;if self.peek()?.is_some_and(|b|b==b'+'||b==b'-'){self.byte()?;}anyhow::ensure!(self.peek()?.is_some_and(|b|b.is_ascii_digit()),"Неверная экспонента");while self.peek()?.is_some_and(|b|b.is_ascii_digit()){self.byte()?;}}},_=>anyhow::bail!("Неверное JSON значение около {}",self.position)
        }Ok(())
    }
    pub fn value(&mut self,depth:usize)->Result<Span>{
        anyhow::ensure!(depth<=128,"JSON глубже128 уровней; исходник сохранён");self.whitespace()?;let start=self.position;
        match self.peek()?.context("Незавершённый JSON")?{
            b'"'=>self.string(|_|Ok(()))?,
            b'['=>{self.byte()?;self.whitespace()?;if self.peek()?!=Some(b']'){loop{self.value(depth+1)?;self.whitespace()?;if self.peek()?!=Some(b','){break;}self.byte()?;}}self.expect(b']')?;},
            b'{'=>{self.byte()?;self.whitespace()?;if self.peek()?!=Some(b'}'){loop{self.string(|_|Ok(()))?;self.whitespace()?;self.expect(b':')?;self.value(depth+1)?;self.whitespace()?;if self.peek()?!=Some(b','){break;}self.byte()?;self.whitespace()?;}}self.expect(b'}')?;},
            _=>self.scalar()?
        }Ok(Span{start,end:self.position})
    }
    /// Object fields are delivered one at a time; unknown values need no allocations.
    pub fn fields(&mut self,mut field:impl FnMut(&str,Span)->Result<()>)->Result<()>{
        self.whitespace()?;self.expect(b'{')?;self.whitespace()?;
        if self.peek()?!=Some(b'}'){loop{let name=self.key()?;self.whitespace()?;self.expect(b':')?;let span=self.value(1)?;field(&name,span)?;self.whitespace()?;if self.peek()?!=Some(b','){break;}self.byte()?;self.whitespace()?;}}
        self.expect(b'}')?;Ok(())
    }
    pub fn array(&mut self,mut item:impl FnMut(Span)->Result<()>)->Result<()>{
        self.whitespace()?;self.expect(b'[')?;self.whitespace()?;
        if self.peek()?!=Some(b']'){loop{let span=self.value(1)?;item(span)?;self.whitespace()?;if self.peek()?!=Some(b','){break;}self.byte()?;}}
        self.expect(b']')?;Ok(())
    }
    pub fn end(&mut self)->Result<()>{self.whitespace()?;anyhow::ensure!(self.peek()?.is_none(),"Данные после JSON объекта");Ok(())}
}
pub fn small(path:&Path,span:Span,max:u64)->Result<serde_json::Value>{anyhow::ensure!(span.end-span.start<=max,"Поле превышает размер служебного пакета; исходник сохранён");let mut file=File::open(path)?;file.seek(SeekFrom::Start(span.start))?;let mut bytes=Vec::new();file.take(span.end-span.start).read_to_end(&mut bytes)?;Ok(serde_json::from_slice(&bytes)?)}

#[cfg(test)]mod tests{
    use super::*;
    #[test]fn strings_are_strict_and_packeted(){let path=std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());let text=format!("{}\0\r\n🦀","я漢字".repeat(30000));std::fs::write(&path,serde_json::to_vec(&text).unwrap()).unwrap();let mut reader=Reader::open(&path,&CancellationToken::new()).unwrap();let mut output=String::new();reader.string(|part|{assert!(part.len()<=16387);output.push_str(part);Ok(())}).unwrap();reader.end().unwrap();assert_eq!(output,text);
        for invalid in [r#""\uD800""#,r#""\uDC00""#,r#""\uD800\u0000""#,r#"[01]"#,r#"{"x":1,}"#,r#"[true false]"#]{std::fs::write(&path,invalid).unwrap();let mut reader=Reader::open(&path,&CancellationToken::new()).unwrap();assert!(reader.value(0).and_then(|_|reader.end()).is_err(),"{invalid}");}std::fs::remove_file(path).unwrap();}
}
