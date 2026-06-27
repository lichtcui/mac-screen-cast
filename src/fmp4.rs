pub struct Fmp4State {
    sps: Vec<u8>,
    pps: Vec<u8>,
    width: u32,
    height: u32,
    timescale: u32,
    sequence_number: u32,
    dts_counter: u64,
}

impl Fmp4State {
    pub fn new(sps: Vec<u8>, pps: Vec<u8>, width: u32, height: u32, timescale: u32) -> Self {
        Fmp4State { sps, pps, width, height, timescale, sequence_number: 1, dts_counter: 0 }
    }

    #[allow(dead_code)]
    pub fn codecs_string(&self) -> String {
        let profile = self.sps.get(1).copied().unwrap_or(0x42);
        let compat = self.sps.get(2).copied().unwrap_or(0x00);
        let level = self.sps.get(3).copied().unwrap_or(0x1e);
        format!("avc1.{:02x}{:02x}{:02x}", profile, compat, level)
    }

    pub fn build_init_segment(&self) -> Vec<u8> {
        let ftyp = self.build_ftyp();
        let moov = self.build_moov();
        let combined = [&ftyp[..], &moov[..]].concat();
        let mut out = Vec::with_capacity(combined.len());
        out.extend_from_slice(&combined);
        out
    }

    pub fn build_media_segment(&mut self, data: &[u8], is_keyframe: bool, _pts_tscale: i32, _pts_val: i64) -> Vec<u8> {
        let seq = self.sequence_number;
        self.sequence_number += 1;

        let dts = self.dts_counter;
        let frame_ticks = self.timescale as u64 / 30;
        self.dts_counter += frame_ticks;
        let cto = 0i32;

        let moof = self.build_moof(seq, dts, data.len(), is_keyframe, cto);
        let moof_size = moof.len();

        // rebuild with correct data_offset
        let data_offset = moof_size as u32 + 8;
        let moof = self.build_moof_with_offset(seq, dts, data.len(), is_keyframe, cto, data_offset);
        let mdat_size = 8 + data.len();

        let mut segment = Vec::with_capacity(moof.len() + mdat_size);
        segment.extend_from_slice(&moof);
        segment.extend_from_slice(&(mdat_size as u32).to_be_bytes());
        segment.extend_from_slice(b"mdat");
        segment.extend_from_slice(data);
        segment
    }

    fn box_full(tag: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let size = 8 + payload.len();
        let mut b = Vec::with_capacity(size);
        b.extend_from_slice(&(size as u32).to_be_bytes());
        b.extend_from_slice(tag);
        b.extend_from_slice(payload);
        b
    }

    fn build_ftyp(&self) -> Vec<u8> {
        let payload = {
            let mut p = Vec::new();
            p.extend_from_slice(b"iso5");
            p.extend_from_slice(&0u32.to_be_bytes());
            p.extend_from_slice(b"iso5");
            p.extend_from_slice(b"iso6");
            p.extend_from_slice(b"mp41");
            p
        };
        Self::box_full(b"ftyp", &payload)
    }

    fn build_moov(&self) -> Vec<u8> {
        let mut p = Vec::new();

        // mvhd
        let mut mvhd = Vec::new();
        mvhd.extend_from_slice(&0u32.to_be_bytes());
        mvhd.extend_from_slice(&0u32.to_be_bytes());
        mvhd.extend_from_slice(&0u32.to_be_bytes());
        mvhd.extend_from_slice(&self.timescale.to_be_bytes());
        mvhd.extend_from_slice(&0u32.to_be_bytes());
        mvhd.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        mvhd.extend_from_slice(&0x0100u16.to_be_bytes());
        mvhd.extend_from_slice(&[0u8; 10]);
        mvhd.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        mvhd.extend_from_slice(&[0u8; 12]);
        mvhd.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        mvhd.extend_from_slice(&[0u8; 12]);
        mvhd.extend_from_slice(&0x4000_0000u32.to_be_bytes());
        mvhd.extend_from_slice(&[0u8; 24]);
        mvhd.extend_from_slice(&2u32.to_be_bytes());
        p.extend_from_slice(&Self::box_full(b"mvhd", &mvhd));

        // trak
        let trak = self.build_trak();
        p.extend_from_slice(&trak);

        // mvex
        let mut trex = Vec::new();
        trex.extend_from_slice(&0u32.to_be_bytes());
        trex.extend_from_slice(&1u32.to_be_bytes());
        trex.extend_from_slice(&1u32.to_be_bytes());
        trex.extend_from_slice(&0u32.to_be_bytes());
        trex.extend_from_slice(&0u32.to_be_bytes());
        trex.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&Self::box_full(b"mvex", &Self::box_full(b"trex", &trex)));

        Self::box_full(b"moov", &p)
    }

    fn build_trak(&self) -> Vec<u8> {
        let mut p = Vec::new();

        // tkhd
        let mut tkhd = Vec::new();
        tkhd.extend_from_slice(&0x0000_0003u32.to_be_bytes());
        tkhd.extend_from_slice(&0u32.to_be_bytes());
        tkhd.extend_from_slice(&0u32.to_be_bytes());
        tkhd.extend_from_slice(&1u32.to_be_bytes());
        tkhd.extend_from_slice(&0u32.to_be_bytes());
        tkhd.extend_from_slice(&0u32.to_be_bytes());
        tkhd.extend_from_slice(&[0u8; 8]);
        tkhd.extend_from_slice(&0u16.to_be_bytes());
        tkhd.extend_from_slice(&0u16.to_be_bytes());
        tkhd.extend_from_slice(&0u16.to_be_bytes());
        tkhd.extend_from_slice(&0u16.to_be_bytes());
        tkhd.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        tkhd.extend_from_slice(&[0u8; 12]);
        tkhd.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        tkhd.extend_from_slice(&[0u8; 12]);
        tkhd.extend_from_slice(&0x4000_0000u32.to_be_bytes());
        tkhd.extend_from_slice(&(self.width << 16).to_be_bytes());
        tkhd.extend_from_slice(&(self.height << 16).to_be_bytes());
        p.extend_from_slice(&Self::box_full(b"tkhd", &tkhd));

        // mdia
        let mdia = self.build_mdia();
        p.extend_from_slice(&mdia);

        Self::box_full(b"trak", &p)
    }

    fn build_mdia(&self) -> Vec<u8> {
        let mut p = Vec::new();

        // mdhd
        let mut mdhd = Vec::new();
        mdhd.extend_from_slice(&0u32.to_be_bytes());
        mdhd.extend_from_slice(&0u32.to_be_bytes());
        mdhd.extend_from_slice(&0u32.to_be_bytes());
        mdhd.extend_from_slice(&self.timescale.to_be_bytes());
        mdhd.extend_from_slice(&0u32.to_be_bytes());
        mdhd.extend_from_slice(&0x55c4u16.to_be_bytes());
        mdhd.extend_from_slice(&0u16.to_be_bytes());
        p.extend_from_slice(&Self::box_full(b"mdhd", &mdhd));

        // hdlr
        let mut hdlr = Vec::new();
        hdlr.extend_from_slice(&0u32.to_be_bytes());
        hdlr.extend_from_slice(&0u32.to_be_bytes());
        hdlr.extend_from_slice(b"vide");
        hdlr.extend_from_slice(&[0u8; 12]);
        hdlr.extend_from_slice(b"VideoHandler\0");
        p.extend_from_slice(&Self::box_full(b"hdlr", &hdlr));

        // minf
        let minf = self.build_minf();
        p.extend_from_slice(&minf);

        Self::box_full(b"mdia", &p)
    }

    fn build_minf(&self) -> Vec<u8> {
        let mut p = Vec::new();

        // vmhd
        let mut vmhd_fixed = Vec::new();
        vmhd_fixed.extend_from_slice(&0x0000_0001u32.to_be_bytes());
        vmhd_fixed.extend_from_slice(&[0u8; 8]);
        p.extend_from_slice(&Self::box_full(b"vmhd", &vmhd_fixed));

        // dinf
        let dref = {
            let mut d = Vec::new();
            d.extend_from_slice(&0u32.to_be_bytes());
            d.extend_from_slice(&1u32.to_be_bytes());
            d.extend_from_slice(&Self::box_full(b"url ", &[0u8, 0, 0, 1]));
            Self::box_full(b"dref", &d)
        };
        p.extend_from_slice(&Self::box_full(b"dinf", &dref));

        // stbl
        let stbl = self.build_stbl();
        p.extend_from_slice(&stbl);

        Self::box_full(b"minf", &p)
    }

    fn build_stbl(&self) -> Vec<u8> {
        let mut p = Vec::new();

        // stsd
        let stsd = self.build_stsd();
        p.extend_from_slice(&stsd);

        // empty stts, stsc, stsz, stco
        let empty4 = |tag: &[u8; 4]| -> Vec<u8> {
            Self::box_full(tag, &[0u8; 8])
        };
        p.extend_from_slice(&empty4(b"stts"));
        p.extend_from_slice(&empty4(b"stsc"));
        let mut stsz = Vec::new();
        stsz.extend_from_slice(&0u32.to_be_bytes());
        stsz.extend_from_slice(&0u32.to_be_bytes());
        stsz.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&Self::box_full(b"stsz", &stsz));
        p.extend_from_slice(&empty4(b"stco"));

        Self::box_full(b"stbl", &p)
    }

    fn build_stsd(&self) -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&1u32.to_be_bytes());

        // avc1 sample entry
        let mut avc1 = Vec::new();
        avc1.extend_from_slice(&[0u8; 6]);
        avc1.extend_from_slice(&1u16.to_be_bytes());
        avc1.extend_from_slice(&0u16.to_be_bytes());
        avc1.extend_from_slice(&0u16.to_be_bytes());
        avc1.extend_from_slice(&[0u8; 12]);
        avc1.extend_from_slice(&(self.width as u16).to_be_bytes());
        avc1.extend_from_slice(&(self.height as u16).to_be_bytes());
        avc1.extend_from_slice(&0x0048_0000u32.to_be_bytes());
        avc1.extend_from_slice(&0x0048_0000u32.to_be_bytes());
        avc1.extend_from_slice(&0u32.to_be_bytes());
        avc1.extend_from_slice(&1u16.to_be_bytes());
        avc1.extend_from_slice(&[0u8; 32]);
        avc1.extend_from_slice(&0x0018u16.to_be_bytes());
        avc1.extend_from_slice(&0xffffu16.to_be_bytes());

        // avcC
        let avcc = self.build_avcc();
        avc1.extend_from_slice(&avcc);

        p.extend_from_slice(&Self::box_full(b"avc1", &avc1));
        Self::box_full(b"stsd", &p)
    }
}

/// Build avcC configuration record from SPS/PPS NAL units.
pub fn build_avcc_data(sps: &[u8], pps: &[u8]) -> Vec<u8> {
    let mut avcc = vec![
        1,
        sps.get(1).copied().unwrap_or(0x42),
        sps.get(2).copied().unwrap_or(0x00),
        sps.get(3).copied().unwrap_or(0x1e),
        0xff,
        0xe1,
    ];
    avcc.extend_from_slice(&(sps.len() as u16).to_be_bytes());
    avcc.extend_from_slice(sps);
    avcc.push(1);
    avcc.extend_from_slice(&(pps.len() as u16).to_be_bytes());
    avcc.extend_from_slice(pps);
    avcc
}

impl Fmp4State {
    fn build_avcc(&self) -> Vec<u8> {
        let p = build_avcc_data(&self.sps, &self.pps);
        Self::box_full(b"avcC", &p)
    }

    fn build_moof(&self, seq: u32, dts: u64, data_len: usize, key: bool, cto: i32) -> Vec<u8> {
        self.build_moof_with_offset(seq, dts, data_len, key, cto, 0)
    }

    fn build_moof_with_offset(&self, seq: u32, dts: u64, data_len: usize, key: bool, cto: i32, data_off: u32) -> Vec<u8> {
        let mut p = Vec::new();

        // mfhd
        let mfhd = {
            let mut b = Vec::new();
            b.extend_from_slice(&0u32.to_be_bytes());
            b.extend_from_slice(&seq.to_be_bytes());
            Self::box_full(b"mfhd", &b)
        };
        p.extend_from_slice(&mfhd);

        // traf
        let traf = self.build_traf(dts, data_len, key, cto, data_off);
        p.extend_from_slice(&traf);

        Self::box_full(b"moof", &p)
    }

    fn build_traf(&self, dts: u64, data_len: usize, key: bool, cto: i32, data_off: u32) -> Vec<u8> {
        let mut p = Vec::new();

        // tfhd
        let tfhd = {
            let mut b = Vec::new();
            b.extend_from_slice(&0x0002_0000u32.to_be_bytes());
            b.extend_from_slice(&1u32.to_be_bytes());
            Self::box_full(b"tfhd", &b)
        };
        p.extend_from_slice(&tfhd);

        // tfdt (version 1 for 64-bit)
        let tfdt = {
            let mut b = Vec::new();
            b.extend_from_slice(&0x0100_0000u32.to_be_bytes());
            b.extend_from_slice(&dts.to_be_bytes());
            Self::box_full(b"tfdt", &b)
        };
        p.extend_from_slice(&tfdt);

        // trun (version 1 for signed CTO)
        let trun_flags: u32 = 0x0001 | 0x0100 | 0x0200 | 0x0400 | 0x0800;
        let trun = {
            let mut b = Vec::new();
            b.extend_from_slice(&(0x0100_0000u32 | trun_flags).to_be_bytes());
            b.extend_from_slice(&1u32.to_be_bytes());
            b.extend_from_slice(&data_off.to_be_bytes());

            let dur = self.timescale as u32 / 30;
            b.extend_from_slice(&dur.to_be_bytes());
            b.extend_from_slice(&(data_len as u32).to_be_bytes());

            let flags = if key { 0x0200_0000u32 } else { 0x0101_0000u32 };
            b.extend_from_slice(&flags.to_be_bytes());
            b.extend_from_slice(&cto.to_be_bytes());
            Self::box_full(b"trun", &b)
        };
        p.extend_from_slice(&trun);

        Self::box_full(b"traf", &p)
    }
}
