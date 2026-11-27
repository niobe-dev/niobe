// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Leonardo's portrait of Lisa Gherardini, as the portrait scene samples it:
//! [`ROWS`] is the painting [`WIDTH`] patches across and [`HEIGHT`] down,
//! each patch a letter naming one of the [`PATCHES`], and each of those the
//! glyph and the inks a cell of that patch is drawn in.
//!
//! The patches were worked out from a public-domain photograph of the
//! painting (the C2RMF's, which Wikimedia Commons holds), scaled to this
//! size and its colours cut to forty-eight by median cut, in order of how
//! light each is. Each patch's glyph says how light it is as painted, from
//! a blank for the darkest to `@` for the lightest, on `.:-=+*#%`: on a dark
//! blue pane a denser glyph reads lighter, so her hair and dress, the
//! darkest of the painting, are sparse and the pane's blue shows through
//! them as their shadow, and she stands dark against the landscape as she
//! does in the panel. Each patch's inks say what it is part of. The
//! landscape is in greens turning to a pale cyan for the sky. Her skin, lit,
//! is cream and gold. Her hair and dress are browns: the photograph's
//! shadows lean violet, which on the blue pane would read as the pane
//! itself, so their hue is turned to the umber they are painted in. In the
//! sixteen names the landscape is green, light green and, for the sky, light
//! cyan; her skin light yellow and, where it is most lit, white; the
//! darkest of her hair and dress red, the rest brown, and their most orange
//! light red — never the blue the pane is.

use ratatui::style::Color;

/// How many patches across the painting is.
pub(super) const WIDTH: usize = 96;

/// How many patches down the painting is: the panel's own proportions, two
/// to three.
pub(super) const HEIGHT: usize = 144;

/// What a patch of the painting is drawn as: a glyph, its ink on a terminal
/// that draws 24-bit colour and its ink in the sixteen names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Patch {
    pub(super) glyph: char,
    pub(super) ink: (u8, u8, u8),
    pub(super) named: Color,
}

impl Patch {
    const fn new(glyph: char, ink: (u8, u8, u8), named: Color) -> Self {
        Self { glyph, ink, named }
    }
}

/// Where in [`PATCHES`] the patch a letter of [`ROWS`] names is: `a` to `z`
/// the first twenty-six, `A` to `V` the rest, from the darkest as painted to
/// the lightest.
pub(super) fn index(letter: u8) -> Option<usize> {
    let at = match letter {
        b'a'..=b'z' => letter - b'a',
        b'A'..=b'Z' => letter - b'A' + 26,
        _ => return None,
    };
    Some(usize::from(at)).filter(|at| *at < PATCHES.len())
}

/// Every patch, in the order its letter names it; each comment gives the
/// letter, what it is part of and its colour as painted.
pub(super) const PATCHES: [Patch; 48] = [
    Patch::new(' ', (124, 62, 18), Color::Red), // a shadow, painted (8, 4, 24)
    Patch::new(' ', (130, 65, 19), Color::Red), // b shadow, painted (12, 6, 27)
    Patch::new(' ', (143, 72, 20), Color::Red), // c shadow, painted (14, 11, 33)
    Patch::new(' ', (137, 69, 20), Color::Red), // d shadow, painted (23, 9, 30)
    Patch::new(' ', (147, 73, 21), Color::Red), // e shadow, painted (26, 13, 34)
    Patch::new('.', (145, 79, 33), Color::Red), // f shadow, painted (23, 17, 37)
    Patch::new('.', (133, 78, 39), Color::Red), // g shadow, painted (33, 17, 30)
    Patch::new('.', (148, 80, 31), Color::Red), // h shadow, painted (32, 17, 38)
    Patch::new('.', (138, 85, 46), Color::Red), // i shadow, painted (31, 21, 38)
    Patch::new('.', (143, 87, 48), Color::Red), // j shadow, painted (33, 23, 40)
    Patch::new('.', (157, 86, 35), Color::Red), // k shadow, painted (44, 20, 31)
    Patch::new('.', (151, 92, 50), Color::Red), // l shadow, painted (35, 27, 44)
    Patch::new('.', (148, 90, 49), Color::Red), // m shadow, painted (42, 26, 38)
    Patch::new(':', (173, 97, 42), Color::Yellow), // n shadow, painted (55, 26, 30)
    Patch::new('.', (158, 97, 53), Color::Yellow), // o shadow, painted (45, 33, 45)
    Patch::new(':', (167, 102, 56), Color::Yellow), // p shadow, painted (53, 33, 39)
    Patch::new(':', (190, 108, 49), Color::Yellow), // q shadow, painted (66, 33, 32)
    Patch::new(':', (175, 107, 58), Color::Yellow), // r shadow, painted (55, 39, 44)
    Patch::new(':', (184, 112, 61), Color::Yellow), // s shadow, painted (59, 43, 45)
    Patch::new(':', (209, 78, 46), Color::Yellow), // t shadow, painted (75, 42, 34)
    Patch::new(':', (191, 98, 64), Color::Yellow), // u shadow, painted (62, 51, 47)
    Patch::new('-', (196, 109, 77), Color::Yellow), // v shadow, painted (76, 54, 46)
    Patch::new('-', (213, 102, 64), Color::Yellow), // w shadow, painted (86, 51, 39)
    Patch::new('-', (228, 113, 65), Color::LightRed), // x shadow, painted (100, 55, 36)
    Patch::new('-', (90, 178, 90), Color::Green), // y landscape, painted (70, 73, 55)
    Patch::new('=', (202, 152, 96), Color::Yellow), // z shadow, painted (88, 71, 52)
    Patch::new('=', (231, 141, 85), Color::LightRed), // A shadow, painted (112, 68, 41)
    Patch::new('=', (102, 188, 102), Color::Green), // B landscape, painted (81, 88, 65)
    Patch::new('=', (227, 166, 95), Color::Yellow), // C shadow, painted (111, 81, 46)
    Patch::new('=', (104, 200, 113), Color::Green), // D landscape, painted (83, 99, 69)
    Patch::new('+', (234, 167, 109), Color::Yellow), // E shadow, painted (129, 83, 43)
    Patch::new('+', (218, 191, 125), Color::Yellow), // F shadow, painted (114, 98, 58)
    Patch::new('+', (110, 209, 129), Color::LightGreen), // G landscape, painted (89, 111, 74)
    Patch::new('+', (237, 186, 128), Color::Yellow), // H shadow, painted (143, 97, 45)
    Patch::new('+', (100, 219, 122), Color::LightGreen), // I landscape, painted (110, 114, 70)
    Patch::new('+', (109, 221, 139), Color::LightGreen), // J landscape, painted (108, 122, 75)
    Patch::new('*', (240, 202, 147), Color::Yellow), // K shadow, painted (152, 111, 51)
    Patch::new('*', (125, 221, 160), Color::LightGreen), // L landscape, painted (118, 131, 83)
    Patch::new('*', (123, 223, 159), Color::LightGreen), // M landscape, painted (128, 132, 82)
    Patch::new('*', (219, 197, 130), Color::LightYellow), // N lit, painted (151, 131, 70)
    Patch::new('*', (134, 223, 174), Color::LightGreen), // O landscape, painted (130, 139, 88)
    Patch::new('#', (237, 196, 128), Color::LightYellow), // P lit, painted (179, 133, 59)
    Patch::new('#', (148, 224, 190), Color::LightGreen), // Q landscape, painted (141, 148, 96)
    Patch::new('#', (151, 228, 196), Color::LightCyan), // R landscape, painted (151, 154, 97)
    Patch::new('%', (236, 209, 152), Color::LightYellow), // S lit, painted (187, 151, 73)
    Patch::new('%', (138, 244, 214), Color::LightCyan), // T landscape, painted (174, 164, 98)
    Patch::new('@', (245, 230, 181), Color::White), // U lit, painted (203, 177, 96)
    Patch::new('@', (255, 238, 194), Color::White), // V lit, painted (233, 196, 103)
];

/// The painting, a row of patches to a line, from the top.
pub(super) const ROWS: [&str; HEIGHT] = [
    "GGDDDDDDDDDDDDDDGDDBDBBDBBDDDDDDGDDDDGGGGGGGDDGGGGDDDDDBDBDDDDBDBBBByByyBBBBBDDDBBBBByDDByyyyyBB",
    "GGGGGGGGGGGGGGGGGGGGGGGGGDGGGGGGGGGGDGJGGGJGGJJGGGGGGGJGGGGGGGDDDDDDDDDDDDDDDDDDGDGGGDGGGGDGDGGG",
    "JJJJJJJJJJJJIJJJJJJJJJJJJJIGGJJJGGGGGIIJJJGGGGJJJJGGGGJJIJGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGJJG",
    "LJJLJJJIIJJJJIIIJJJJLJJIJJIIIJJJJJJJJIJIJJJJJJJJJJJJJJJIIIJJJIJJJIIGGGJJJJJJJJGGJJJJJJJJLJLLLLLG",
    "LJLLJJJIIJJJIIJJJJJJJJJJJIIJJIIIJJJJIIJIIIJJJJJJJJJJJJJJJIJJJIIJIIIIIIIIJJIJJJGIIJJJJJJJJJJLLLJG",
    "LLLJJJJIIJIJJJJJJLLIIIJIJIIIIIIIIJJJJJIIIIJJJJJJJJJJJJJJJJLJJJJJIJIJJJJJJJJJJIJJJLJJJLJLLLJLLJJG",
    "LLLJLLJJJJJJIIIJJJJIIJIIIIIIIIIJJJJJJJIJIJJJJJJLLJJJJJJJJJLLLJLLJJJJJJJJJJJJIJJJLLLLLLLLLLLLLJJI",
    "JMMLLLJLLLJJJJJLJJJJLLJIIIIIIJJJJJJJJJJJLLLLLLLLLLLJJJJJJLLLLLLLLLLLJJJLLLLLLJJLLOOLLLLLLOLOOMLG",
    "IMOOLLLOMLLLLLLLLMLLLLLJLJJJJJJJLJLMLJJJLMMLLLLLLLLLJLJJJLLLLLLLLLLLJLLLLLLLLLLLOOOLLLLLOOMOOMLJ",
    "LMOMLLLLLLLMLLOLOOMLLLLJLLLLLLJLLLLMLLLJLMMMMOOOOQOLLLLLLLLLLOOMLLLLLLLLLLLLLLLOOLOOLLLLLLLOOOOJ",
    "MMOLMLLLMMLLMOOOMOOOMMMMMMMLLLLLMMMMMMMMLMLLOOMOOOOOOMLLLLLLOOOOOMLLLMOLMMMOMLOOOOOOOOOMMMOOOOOJ",
    "MOOOLMLLMMMMOOOOMOOOOOOOMMMMMMMMOOMMMMMMMMMOOMMOOOOOMMMLLOOOOOOOOMMMMMOLMMMOOMOOOOOOOOOOMOOQOOOJ",
    "MQOLMMMMOOOOOOOOMOOOOOQOMOOOOMMOOOMOMMMMMMOOQQOOOQQOOMMLLMOOOOOOOLMMMMMMMMMMOMOOOOOOQOOOMOOQQQQL",
    "MQQOMOOOQOOOOOOMOOOOOMOOOOOMMOMOOOOOOOMOOQOMIFCCCCFIMMMLLMMOOMOOOMMMMMMOMMMOOOOOOOOQQOOOMOOOQQQJ",
    "OQQOOQQQQOOOOOOMOOOOOOQOOQQOMMMMOOOOQOORQKCxtttttqqqtwFMOMMOOOQOOOMMMMMOMOOOOOOOQOOQQQQOMOQQQQQL",
    "MQQOQOQQQOOOOOQOOOOOOOQQQQQQOOQQOQOOQQQNAtxAxtttqqqqtqnqCMMOOOQOOMMMOMMOOOOOOOOOQQQQRQQQOOQQQQQL",
    "MQQQOOQQOOOOQQQOQOOOQQQQQQQQQQRQQQQQQRFxxxEExxwttqnknqtngtMOOQQQOOOOOOOOOOQQQOOQQQQRRQQQQQQQQRQM",
    "MQQQOOOQQQQQRRQOQQQQQQRQQQQQQRRRRQQRRHxxAAHEAAxxwtqnkgknndkIQOQQQQQOOOOQQQQQQQQQQQQQQQQQRQQQRRQQ",
    "MQQOQQQOQQQRRRRQRQQQRRRQRRQQRRQRQQRRIxAAAEHEAAxxwttqnkggkkdkMQQQQQOOOOOOQQQQQQRRRRRRQQQQQQQQRRQQ",
    "MQQQQRQRRRRRQRRQRQRRRRRRRRQQRRRRRRRQxxAEHKPKHEEAwttqnnkggkkbpORQQQQOQQQOQQQQQQRRRRRRQRQQRQQQRRRQ",
    "MOQQRRRRRRRRRRRQRRRRRRRRRRRQRRRRRRRFxEKPSUSSSPPKExtqqnkgggggbzRQQQQQQQQQQQQQRRRRRRRRRRRRRQRRRRQO",
    "MQQQQRRRRRRRRQRQRRRRRRRRRRRRRRRRRRMwEPSUUUUUUSSPKExwqnkggggggdFRQQQQRRQQRRRRRRRRRRRRRQRQRRRRRRRQ",
    "MQQQQQRRRRRRRRRRRRRRRRRRRRRRRRTRRRCwKSUVVVVVVUUSPKExtqngggggednQRQQQQRRRRRRRRRRRRRRRRRQQRRRRRRRQ",
    "OQRQQRRRRRRRRRRRRRRRTRTRRTRRRRRRRIqCPUVVVVVVVVUSSPHAwtnkggdgdddFRRQRQQRRRRRRRRRRTTRRRRRRRRRRRRRQ",
    "QRRRRRQQRRRTTRTTRRRTTTTRRTTRRRRRRwqHSUVVVVVVVVUUSPHExtqnngdgddbsRRQQQRRRRRRTRRRRTTTTRRRRRRRRTTRR",
    "MQRRRTRRRRTTTTTTTTTTTTTRTTTRRTTTMwqHSUVVVVVVVUUSPPKEAwqnngggdddgMQOOQRQQRRRRRRRRTTTTTTTRRRRTTTTR",
    "JQRRRTRRQRTTTTTTTTTTTRRRTTTRRTTQFttHSUUUUUUUUUSSPPKHEAwnggggdddbBLJOOOMOQRQRRRRRTRTTTTTTRRRRTTRR",
    "IORQRRRQOQTRTTTTTTTTTTRTTTRTTTTKxqwKSUUUUVUUUUSSSPPKKHAqkkgggddduJIMLMLORQORORRRTTTTTTTTTRRRTTTQ",
    "ILOOQOOMIMTRTTTRRRTTTTRTTTTTTTRCtnxPSUUUUUUUUUUSSSSPPKAtkkgggdddmIDIIIIOQOQQORRTTTTTTTTTTRQRTTRQ",
    "IJMLJIMMIIQRRTQNQQRTTTTTTTTTTTNAtnxPSUUUUUUUUUSSSSSPPKAtkggdeggdgGBDIGILLLOLLRTTTTTTTTTTRROQRROL",
    "yIMJGDILMIJMMMQNMLOQQTTTTTTTUTNCqpxPSSSSUUUUSKKPPPPKKHAtkkgeemgdgDyBIJJJGJLJJQRTTTTTTTTRROMQOOJJ",
    "uyDGDDIJILMMMILMILJJLMRTTTTTTRFzqqwPPKKHSUSPEAEKHEECAAAtnkgheggggyyBBDJGGJLJJMQTTTTTTUTTRQQQLOOL",
    "syBDDDILIIMMIIIIDIIIMJJTTTTTTQCzwtwAEKHHKSSKAEKKECxtwCCwnkkheeggeyBBBBGGGJGDJLOTTTTTTTTTRRQOOROL",
    "uyBDGDGIIIJIIFIIGDIIMJJTTTTTRQFtttxEEKxAKSUEEKSHxCEEKPHxnmmgfeggguByyyDDDGDBDJMTTTTTTTTRRQOOQROO",
    "uyBDGGIIIJIIIIIFBDGIMDOTTOIOQRCttnAPPSPKPUUHHPSSPKKSSPHxnkmheeggduyyyBBDDDDDGGLTTTTTTTTRRQMOQROL",
    "uyDDDGJGGJJFFIIBBDDFIBLQNFGDOMzttkASSSPPSUUKKSSPPPSSSPHwkkgidegkeoyyyyDGDDDDGGJQTTTTTRRRRQMOOQOL",
    "uyBDBDGGGGGFDBDBBDByBBBFFFIBIFCqqkAUUUUUSUSPPSVVVVUSPKEtkgggeegkgmyyyBDGDDBBBDGOTTTTRRRRRQOQQOMO",
    "yBDBFDDDGGDBBFzBBDByBByzzCFyBCCtqkxSSUUUUUSPPSUVVUUSPKAqkkgkmggjgjBBByBBBBByBBDJQRRRRRRRQQOQOOLO",
    "yIGyBDBBDDDyBDByBByyByyzBzByzzwttntPUUUUUUSPPSUUUUSPKExqkkkmkieigmIIGIJJJJJJJJJJLOQQONNOMMMMOLLL",
    "yFyyuyBByBBBDGDBByyyyyyzyyyyvvtqqqqPUUUUSUSPKSUUUSSPHExnkkkmmgeegguyyBGGGGJJLOOOOQQQOMLJIMOOLMID",
    "zDByuyBBBBBBBDFDDBByyBBBzyyyusttqknHSUUSSVSPKPUUUSPKEAwnkkkmmgieimruuuyyyyBBBDGGLJJJGDIDBGLGDDBD",
    "yBzyuyBByBBBBBBDDDBBBBzzzzyyusqqqnkAPSSSPSHAAKUUSSKHEAtnkkgmmgjffmouuyyyyBByyByDGGDGDGGBBBByBDBD",
    "uByuuuyBByBBBByyDDBBByBzyzyyustnnnkxKPSUUHxxAPSSPPKHEAtnkkkmgiifimpuuuyyyyyyyBBByBBDDDDDBBBBBDDJ",
    "uuuuyuyByBBBzzzyBByyBBBByzyyyppqnnntKKPSVSHHKPKKPPKHEAtnkkkkmmgefmpyyyyyyyyyyyyyyyyBDGDDBBBBBBBJ",
    "osuuyuyBBBByzBByBBByzzBzyyyyyspqttnqHPPKPPKHEEEPPPKHEAtnkkkkggeefmpuyyyyyyyyyyyBBDDGIGDDByyBBDDD",
    "uuyyuyBBBBzzzBBBByyyBzyyzyyyyvtttnqkAPPSSSPKHHPPSPKEAxqnkkkmkgeejmmuyyyyyyyyBBBBBDGGDDDByBBBBBDG",
    "uuyuuBBByBBBzzByyyyyyyyyyyyByvvtqnnknHPSUPKKKKPPPKHCAwqnkkmkkgeemmpuyyyBBBBBBBBBBDDBByBBBBBBBDGG",
    "ruvuuyzzzyyyzzzyyyyyyyyyyyBBzvvqnnnnktHPSSPPPKKKKHExxwqnkkmgggeeimpvByyyBBBByyBBBByyyyByBDBBBDDB",
    "rsuvvyyzBzBzzyzzyyyyBBBBBBBDDzwtnnnnnkqHSUVUSPKKHCxxwtnkkkkgggggjmpvByyBBDBByyyBBBByBBBBBGBBBBBB",
    "ssruvuuuyyBByyyyyyyyyBBBBBBBFzwtqqqqnnntKUUSPPKEAxxxwqnkkkkkkgggjmpuBBBBIFDByBBBBDBBBBBBBBByBDDy",
    "rsruvuuuryBByyyyyyyyyyyBBBBBGztttnnqnnnntKPKKHAxxxwtqqnnkgkgmkggjgnryyByBDDBBBBBBBByyBBBDDByBDBB",
    "oruuvyyyuyBBByyyyyyyyyyBBDIJLDsttnknkgknptxxxxxxwwqqqttnnkkkkggeiimoyyyyyBByyByyyyByyyBBDGGBDDDB",
    "osuuvyuuuuyByyyyyyyBBDBDDDIIJIvqnnnnkgkknnpttttttqqttttttnkgkgggggijuyyyyByyyyyyyyyyyyyBBBBDDDBy",
    "rsuuuuouzyyBBBDBBDDDDDGJLJJIJIztnnnnnnknnnqAAxwttttwwwxxwqkkggggkgiluyyyBBBByyyyyyyyyyyyBByBDGBB",
    "rruuuuruyuuzBBByBDDDGGGGJJIGIGBspqqnnkknpnqHKECAwwxxACCAxqnnkgggkkgpyyyyyyyyyyyBByyyyByyByyBGIFB",
    "ssuuuyuuuuyzBBGDDGGGGGDGGJIIIIDvnqqqnknnnnqCPKHECAAACEEEAtnnkgggkkgguyBBGIDBDDBDDDDDFFIIBBBDGIFB",
    "sryyyyyuyyBBBDGDDGGGGGGGGGGGGDBuqnqqnnnnnqqCSPKKHEEEHHKHExqnkggknkgdmyyBFDBGGDGLGDIIIMNIFFDIDIFB",
    "uuyyyBBBBBBBDGBzBDzzBJIGGGGGIyBsqnnnnnnnnttAPPPPKKKKKKKKHAtqnknkqngegporuuuyBIIDDFKKFFFBFIFBBFFB",
    "uuyBBBDDDGDGIFFCzzzzvyBFCzFzvzvppnknnnnqttAHNPSPPPPPPPPPKEwqqnqnwqggekkpuuuszyupyINNIMFFIFBBBBFB",
    "yyyBDBBBBGDDzFFCzzzFzpsvzsvvnuvppnknnqqtxENSSSSSSSPSSSSPPHxtqttqtnimggmrsuuuususvzKKFFFFIIFBBBFB",
    "zzyBBBDDBGGCFCCxACzCzstpuvvvspqmnnnnnttAKPSUSSSSSSSSUSSSPKEwqqxttnkmmgmuuruyyBBzFFNNMzFFIIDBBFFB",
    "zxvyBDDyzzzCFCxwxCzzzpssppvzupmmnnqqqwHPPSSUUUUSSUUUUUSSSPExttxwwnpmkkksyzFFMMMNNNNNFFFFIFBBBFFB",
    "zxAzzBByzzCCCAAxACCCvptvppvvppgmnnqqwHPSUUUUVUUUUUUUUUUSPKAwtxAwtnnnnmkkuvzCINNNNKKFIIFFFBBFFBFB",
    "zAAAAwxvvCCECCCEEFCCvqtznmvvtnjmnnqtAPSUUVVVVVVUUUUVUUUSSKAxxxAttknnnnnknuvvzNNNNFKFFFFFFIIIFBFB",
    "zCACAxAwzCECCCCEEEAxttwztmvwtnjmpntxHSUUVVVVVVVUUVVVVUUSSPAAxtxtnknnnqqnkpuvzFMMNNNNNKKFNNMIFFBB",
    "zCAAAACAxEECFCCEECxxxwwxwtwxtkmkntxAKUUUVVVVVVVUVVVVVUUSPKExxtAtnknnntqqnksvCCFEFKKKFFKNNNFFIFFF",
    "zCzCCAEAxCHHHFHKEEAAxwzAwwxwnjmpqxxHPUVVVVVVVVVVVVVVVUUSSKxxxwxtkgknwAxwwnmpzzCFFKKNCFKIFFFIIFFF",
    "zCACCxCAxxKKKKKHEHCxAzCCxxtmekmnwAEHSUVVVVVVVVVVVVVVUUUSPHAxxxxqgntxAAwwtngkqwCFIMNNNNNNRNNNNFFB",
    "zCCzxxCxwzCCKPKKHKHCxxCCxtgimknqAHHKUVVVVVVVVVVVVVVVVUUSPHEAxxwqtxAAACCAxwpkgmtxACFNONNRRTSTNNFF",
    "zAAAxAxwwzAxCEKNKHEEAxAAtmjpnmmwHKPUVVVVVVVVVVVVVVVVVUUSPHAxwwxEHHHHHHHCAwqppjirwwwAFFFFNMNMMNNM",
    "wAAEACAxACAAtwAHHKHECCAwgmoppmnAKSSUVVVVVVVVVVVVVVVVVUUSPEAxxAHHEHKPKKHCxtmmmljhptzxwzzFzzFFINNN",
    "vAEEACEEAAAxxAACHHEHHHAmllpppmtEPSUVVVVVVVVVVVVVVVVVVVSSPHEEEEEHPPPHAxtqqmmjifomhswwwwvzFMMMQNNM",
    "vzCACEEEAACCAAAEEAAEKKtljmppmmtAPUVVVVVVVVVVVVVVVVVVVVUSPKHEEKPPHxtqqpmmjiiiihimomtxwwwvzFNSSSNM",
    "vACCAAAxAEEEAACCAEEHKCmjmmpmknwxHUVVVVVVVVVVVVVVVVVVVVUSPKEEKHAwqnqppggjiihiifjlmjmwwvvwzwzFNMML",
    "vzEEAxAAAEEHEHHHKKKKEpmmjmppntwAAKSVVVVUUVVVVVVVVVVVVVUPKHECxtqptqnmmgedfffffiljmmjsxxxwvzCzzFFK",
    "uuAAEAEHEKKHHHKKKHExmmojhpqkptwCEEEKSUUUVVVVVVVVVVVVVUPKHExqqppsmgffiifcffcffjllloloxAAAAzACAzEH",
    "usxEAEHHHHHEEHEEEExmmomilsppttxCACEAEHHPSVVVVVVVVVVVUPEEExqqsrpmifmopojiffcffilllloovAAEAAzCCzzF",
    "vswAAEHHKEEEHHEEECplroojorprwzCCAAAEEAAEEHKPSSUUUUUPKHEAxttpmmlmpmvsproieffijfjjlooosAAEEAAAECzB",
    "uswAEEEEHHEEEHEEEAmorrroorptwzzxxwxAAAEEHHEEEHHHHKKEFCwwztmjiiirsvvrspmgefffifijloooovwxAwxAEEzz",
    "usrtAEEHHHHHHKKKKAmrrroooorvwwwzttCxwCCEEEEEEEEEEFCFCtwzpddfjjmrvzurspmiefcfffiljlooouvwwvvzEHzB",
    "uusowAAEHHHHHHHKKArsrrooropvzvwzrvzwwCCFCCECCCCCCCCCwvumiffimprrvzpusmikmmmoffiojlooorwxwxxzzzzz",
    "uurmsxxAAEEEEEEEAvrrrrooropvvqwtpvttzCCCCCCCCCAzzCzzvsmgfffiprssvsrvppzzvqvspioojllorowAAxAAzzvy",
    "yusmpwAAAEEEAAAAAtrooooororvvqtpsvpvvzCCCCCAxzwzzzzspmmjffjmpsssspuuvCCwtwtqqrrjjlooorvCCACzzzzz",
    "uvuoowxAAAAAAAEAAtroololoorvusspuvsuvzzCCzzwwvzCvvppmjiffgmmprmrmssvzxxwqtttsoljlmmoorrAEEEzzzBC",
    "uusrrxAAEEAAAEAxwsroolllmorusupmuursvvvzwzwwvwzvvvupgjffejmmpkmmkuttxAwwwxtqrifjlooloolsvwAvvvzz",
    "uvvsvEEHHKHHHHExvsrrolommoosrsppuspsvuuvvvvtvzvvvvpmmmfijmmmpgggnttwCAxAwxqmeffjjoroooorvvzvvzyz",
    "vvzszKFHKKKKNKKFzurrololororprprsporssuuvvtuzvvzvpmmmiiimmjomgggqttxAzxwtrifijijflrrrrrszzCzvyBy",
    "zvzvFKKKKKNKKKHCsrrooorlooororprspmrssvussszvvzvspmpiijijijmiijpvttwAwwwmeffifijjlsurosszCCFzyBz",
    "yuvCKKKKKKKKKKKvorrooorooorrorprspmrssurrpvvvztsrrpmfijjiijjjlostvtwAwtkdeeeiifjllsssrrsvCCFFzzz",
    "zzCHHHKKKHKKKKCrsrrrroorrooropmrspmsrssrprvvvurssroiiiiiiiliijsstwtwwtgdggeeiijjjlruusrsuCEEHEFC",
    "FCCCCCFFFFFHECurrssuussrolospomrrpmrrssrpuvvvssssrjjjiiifijiimrpwqqspmhegkheejjlllosssssrzCzACCC",
    "Bvvvvvzzzzzzzurroossvusroooooomprpmorrrrsvsvuusssoijififijiiimpsqppmmiedkgehhjiljlooroooouuuuuyu",
    "Buuvvvzzzzvvssoroossvvusrooooomopomooropusuvssrsrmjjjiififfffirpmpmiffeekmejgiillloooloooruuuvyv",
    "Byvvzzzzzzzuroroorsuuuusrrorooommmmoopprusuussspmjljiiififcffjmpmkgfeedggggeeiijlloooooooruusvyv",
    "Bzzzzzzzzvurrrsrssuvvvvvsropmhjijmooormrsrussurmmjjjijifffcfiippgcfeiegkkggehiiilloooooooruuvvzz",
    "Bzvzzzzzurrssusrussvvvvvspppmhjimmoooomsruusvrmmjjjljjiffffifipmccfffhgkgghhehifjloooooooruvvzzz",
    "Bvvvvzzrrsosuursuvvvzzvsppmmmhjjijoommppsvvupmmjijjmjiiffffffimecbcfeggggdeeeehiiloooooooruuuvzv",
    "Bvvvvzuosorusustvvvvvvvqsqpmmjhmmjjommopsvvppmmjijjmjiiffffififccccdgkkkgddeegghflrooooooruvvzzz",
    "zzzvvvrurrsusswtssrrppqsrsppijhpmjjijmorvvpommjiiijmiiijfffijjfcccdegkkkgkkmeehijloooloooruvzvzz",
    "zzvvvousousrppssooprsrppmpppihhmmlljjmrusrrpmoijiijjiiiifffflifcccdekkkkkkkkgfegijllloooorsuzzzz",
    "Bzzvsruruuroprolmmmmrmjlpppmihjmjjihmrsspppmomjjijiiiijfffffifcccfeheggkknnkggffilllllooooruzzzz",
    "Bvvsorruuroprovtqtwxtqnmlrpmmiiijhhjmpppoppoomjhhiffffifffcfffcccfdeggggkknkgeeeilllllloooruzzzz",
    "BzvsoorurootpwxxAAxwwxwqmmpmhijjhhiimmmmppommjiiiiefffifcccfcccccceekkkggkgggecffmjlolllolorvzzz",
    "FzuroorroostwxAExqqxxAxqpjjoiijjmjimmijommjmjiiiifffiiffccffccccccegkkhedddeeefcfjiilllooloouuvz",
    "zvuooroorlstxwAxqxExwwwqppmiijjhjjmpmjlljjhijjiiifififfcccfccccccfjedeggkkgehghfffjillljloooruyz",
    "zvvoloooolrwwwxwEKAxEExAExmppsvvsmjmmmmijjjiiiiiiiiiffccccccccfcfehheehkkkeehgieffiiolllloloosuy",
    "zvuooooojluvwCAHKxqACAAAEtCEPSSSPKCsmiiljhhhffiiiijiffccccfcfffcfceeeehkkheeeeefcfjflllolloooouy",
    "zvrooooollvvAEACxtEEwCAHwHKSUUUSSSPPHxpiehjhfffffjfffcccccffffcfcfeehgkkkgheecccffjillllllooouuy",
    "zsoooooollsqxAEwtEKqxAxwAKPUUUUUSSSSPPKEzphhhfiiijffffccccfcccccccehjhmmmheeffffjljllllllloorruy",
    "zsrooooollrswExwACtvAwwtEHSSUUUSSSSSSPPSSPAjfjjjiifijfcccccccccffcfffhhhhhefffiffjllolollloooruy",
    "zsrooooolloswxsxEtrwtxtwEKSSSSSSSSSSSSPPPPPCpfffjfiiffcccccccfccccccfhhhmheeefooijfoolooolorrsuy",
    "zsoooooolllssrrAxttvwxtxAHPPSSSSSSSSSSSPPPPKKEufffffccccccbcccccccccfeehmheeeijjflllllloloooorsy",
    "zrooorrolffopppwtptwwwvxwEPPPPPPPSSSSSSSPPPPPPKHtffcccccccccccbaaaabcbcegheeccfflllljllollooooru",
    "zsooooolliflppppqpttqvswxxHKKKKPPPPSSSSSSSSSSPPKHAwiccbaacbcfgqtqnkkkkcceeehhfflyljjoflojlooooru",
    "zslllllllffjmmoppnprpsoswtAEEHHKPPPPPSSPSSSPPSSSKAtmccmwwtmgqxEEAxAxwtqgcccjjjioyyljoflljloooosu",
    "vsoljhiljfffjlommmmpprmmtvvwwxEKKPPPPPPPPPPKHPSSSKAEEAEKHEEExtxAAxtxxxxqgdcfffflByoljlillloooruu",
    "zwojhheefffffijjhhjoroommuspsCEKKKPPPPPPPPPKExHPPPKAEAEEKExxEAxxAExttqqtnedbfffhsljlillllllllrsu",
    "CwolfhjhffffcfffeffjmmmmjmvCKPKHKKKKPPPPPPPPPxqAHKKEAAAxxAxxxxtxAAExttttqkeccccfjfjijlillllllpsu",
    "volijjlifcfifjffeeeffhhevHKKPPPHHHKKKKKPKHKPSPAttAHEEAEAxxAxxAxttxAxqqttqngdccccffjjffjlijllorsu",
    "ooljlllfffjjjfhffheeefhCKKHHPSPKEEHHKKKPPHAKPPPEwtxAEEAAxtxAxxAxtnqwttttqqkdccfccfifffjlifljotvv",
    "oolijjfffffijjhffffffcwKEEEKPPPPKAEEHHKKPPExHKPPAxwtxEExxttxxxAxttqntnnqqtndbccbcciffflliilootvv",
    "ollfflffifihfhhifffffcxEAAKPKPSPKHAAEEEEKPPHwEKKHAtwqtxtwtttqtxAtntqqnkknngdcbbcccfffcljijlootwv",
    "oljffilijijjhhhihfffeeAEAHPKKPSPPKEAEEEAAKPPHtAEHKxttqntqqqtnntwtnnqnnkkkkdbbbccccfffcjiejoootwv",
    "lljfffjjhhhhijhhjhhhekEAEKHEKPPPKHHAxAHExAHPPEtxEHHAntttnnqqkkqqqnnnqnkggkgbbbbcbcfcfefhjloojtwv",
    "llffffjjjhhhjhmjhhhhhpEAKHEEPKPPKEEAxwAEEwxEHEAtwAHHwnqqknnnkknnnkkknnkgdgdbbabbbccffjijlloooqvv",
    "lffcfeeehjljjhhhehmhhpAAEEAHKKPKEAAxxtqwAxxwxAAxtqAEEqnngknkggkkgggggkkdddbbaaabbcdegijfehoompvv",
    "lfffcccfffffffffehmjoptxAAEKKPHEAxtttqnnqtwtqqtxxqktAqgggkkgggggdgggeeddbdbbbbbbcdeehefhmhmompqs",
    "lffcffhheffffcfceehjmoptxAEHPKAwttqttqtttttttqnqqqggkecddgdddddddddddddbbbbbbbbbddegeeehmmmomppr",
    "lecfhhjiifffcceehhehmmoqwxAKHAwqtttwttwwttwwwttqnnnnkgedddbbcbbbbbbdbbbbbbabbbabddeddeehmjhmjmpp",
    "lihhhhhfiiheehjehhehmmmkntxHAxnnqqqpnknnnnqqqnkkkkkkkkkgdddbbbbbbdbdbbbbbbbaaaabbcdgefhgmjhmhmpp",
    "oljhjhhillhhhhhfihhhmmhhmkwAxtnknqnkmkmnpprspmmkkgggggggdddddbbbbbbbbbbbbbaababbbcekkfhhhijmimpo",
    "ljhfhhhhmecdeefiihehheehmktAwqnnnnnnnmkpqqppnkkkgggggggedddddbabaaabbbbabbabaaaabccgkdiehhhmimpo",
    "jhffhefheccehhheehhheeggkkqxtnnkkkknnnpqqqnnkkkkkkkkkggdddbbbaaaaaaababbaaaaaaaabccggeeeeehhhmrm",
    "lhfffffcfeffefhhhhheeegkkkkqqkgkgkknnnnnnnnkkgggeeeddedbbbbbbaaaaaabbbbbaaaaaaaaaccekgdeeehhejrm",
    "jffeehfcccccccfeheccehgkgknkkgkgegkgkgggkkgdddbbbbabbbbbbbbbbaaaaaaaaaaaaaaaaaaaaabdnkdedeehfjoh",
    "leeeceecccddeeeeddddeggkmknkgggdddddggdggdddddbbbbbbabaabbabbaaaaaabaaaaaaaaaaaaaaabkkddddeefhmh",
    "leeeeecccddeeeededeeeggkkggkgegdddddgggggddddbbbbbaaabaaaaabaaaaaabbaaaaaaaaaaaaababggbdddddcekh",
    "lfheeffedeehecccddddeggdgddgdeggddggkkdkgddddbbbbbababaaabbbbaaaaaaaaaaaaaaaaaaaabadddbdbbdddgmh",
    "jefcceccccehdccccdggggddedddbdedegedgkddbbbdddbabaaaaaaaaaabaaaaaaaaaaaaaaaaaaaaaabbdbbdbddbdgmh",
    "fefcccccfcddcecdegggggggddddbbddeeddgkdddbbbddbbbaaaaaaaaabbaaaaaaaaaaaaaaaaaaaaaaabdbdgbddddghe",
    "fdecccdddccccdddegggghgggeeedbddedgkgkgdddgddbbcbbaabbaaabddbaaaaaaaaaaabbaaaaaaaaadbbdgddddddhh",
    "fcededdecccbccddghhghhdddddddddeegggkkddbdddbbbbbbabbbbbbddddaaaaaaaaaaadbaaaaaaabbbbddgdbbdddef",
    "lccceddccfccdddehhhheedeccdedeeeegedkkgeedbbcbcbbbbbbdcbdddbbaccbaaaaaaabbaaaaaaaaaabdbbbbbdddci",
    "occfffcfcffhcfffccffffohffecfeedeehhmmhmhehheeeffccccfccdeecccffccbbcbccbcabbababaaaababbccccccr",
];
