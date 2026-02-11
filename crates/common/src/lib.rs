use std::io;

/// Resource payload that can be either borrowed from mapped bytes or owned.
#[derive(Clone, Debug)]
pub enum ResourceData<'a> {
    Borrowed(&'a [u8]),
    Owned(Vec<u8>),
}

impl<'a> ResourceData<'a> {
    pub fn as_slice(&self) -> &[u8] {
        match self {
            Self::Borrowed(slice) => slice,
            Self::Owned(buf) => buf.as_slice(),
        }
    }

    pub fn into_owned(self) -> Vec<u8> {
        match self {
            Self::Borrowed(slice) => slice.to_vec(),
            Self::Owned(buf) => buf,
        }
    }
}

impl AsRef<[u8]> for ResourceData<'_> {
    fn as_ref(&self) -> &[u8] {
        self.as_slice()
    }
}

/// Output sink used by `read_into`/`load_into` APIs.
pub trait OutputBuffer {
    /// Writes the full payload to the sink, replacing any previous content.
    fn write_exact(&mut self, data: &[u8]) -> io::Result<()>;
}

impl OutputBuffer for Vec<u8> {
    fn write_exact(&mut self, data: &[u8]) -> io::Result<()> {
        self.clear();
        self.extend_from_slice(data);
        Ok(())
    }
}
