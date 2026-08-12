//! Every import library the SDK ships, as DLL names to ask the handset about.
//!
//! GENERATED — see the test at the bottom of this file for how to regenerate. Committed
//! rather than read at runtime because the question is "what does this handset have",
//! and a list read from the handset would answer itself.
//!
//! # Why the whole list and not a shortlist
//!
//! Because a shortlist answers whoever wrote it. `docs/device-notes.md` is a record of
//! guesses the hardware refused, and the expensive ones were all cases where nobody
//! thought to ask. `RLibrary::Load` costs milliseconds; a name nobody wanted costs one
//! line in the report, and a name nobody asked for costs another trip to the phone.
//!
//! # Why RLibrary::Load and not a file check
//!
//! A DLL can be present on disk and still fail to load — a wrong UID, unsatisfied
//! imports of its own, a capability we do not hold — and every one of those breaks an
//! import exactly as thoroughly as the file being absent. Loading it is the only test
//! that answers the question being asked.

/// The names, lower-cased and de-duplicated, in sorted order.
pub const NAMES: &[&str] = &[
    "agentdialog.dll",
    "aknicon.dll",
    "akninputlanguage.dll",
    "aknnotify.dll",
    "aknpictograph.dll",
    "aknskins.dll",
    "aknskinsrv.dll",
    "aknswallpaperutils.dll",
    "alarmclient.dll",
    "alarmshared.dll",
    "animation.dll",
    "animationshared.dll",
    "apengine.dll",
    "apfile.dll",
    "apgrfx.dll",
    "apmime.dll",
    "apparc.dll",
    "apsettingshandlerui.dll",
    "asn1.dll",
    "audioequalizereffect.dll",
    "audioequalizerutility.dll",
    "avkon.dll",
    "bafl.dll",
    "bassboosteffect.dll",
    "bcardeng.dll",
    "bifu.dll",
    "bioc.dll",
    "biodb.dll",
    "bios.dll",
    "bitgdi.dll",
    "bitmaptransforms.dll",
    "biut.dll",
    "bluetooth.dll",
    "bmpanim.dll",
    "bnf.dll",
    "browserengine.dll",
    "btcmtm.dll",
    "btdevice.dll",
    "btextnotifiers.dll",
    "btmanclient.dll",
    "c32.dll",
    "caf.dll",
    "cafutils.dll",
    "caleninterimutils.dll",
    "caleninterimutils2.dll",
    "calinterimapi.dll",
    "ccon.dll",
    "cenrepnotifhandler.dll",
    "centralrepository.dll",
    "certstore.dll",
    "charconv.dll",
    "clkdatetimeview.dll",
    "clock.dll",
    "cmmanager.dll",
    "cntmodel.dll",
    "cntview.dll",
    "commdb.dll",
    "commondialogs.dll",
    "commonengine.dll",
    "commonui.dll",
    "commsdat.dll",
    "conarc.dll",
    "cone.dll",
    "connmon.dll",
    "contentlistingframework.dll",
    "convnames.dll",
    "convutils.dll",
    "crypto.dll",
    "ctframework.dll",
    "dfpaeabi.dll",
    "dfprvct2_2.dll",
    "dial.dll",
    "directorylocalizer.dll",
    "distanceattenuationeffect.dll",
    "dopplerbase.dll",
    "downloadmgr.dll",
    "downloadmgruilib.dll",
    "drmaudioplayutility.dll",
    "drmhelper.dll",
    "drmlicensechecker.dll",
    "drtaeabi.dll",
    "drtrvct2_2.dll",
    "dtdmdl.dll",
    "ecam.dll",
    "ecmtclient.dll",
    "ecom.dll",
    "econs.dll",
    "edbms.dll",
    "effectbase.dll",
    "efile.dll",
    "efsrv.dll",
    "egul.dll",
    "eikcdlg.dll",
    "eikcoctl.dll",
    "eikcore.dll",
    "eikctl.dll",
    "eikdlg.dll",
    "eiksrv.dll",
    "eiksrvc.dll",
    "ektran.dll",
    "environmentalreverbeffect.dll",
    "environmentalreverbutility.dll",
    "eposlandmarks.dll",
    "eposlmdbmanlib.dll",
    "eposlmmultidbsearch.dll",
    "eposlmsearchlib.dll",
    "esock.dll",
    "esocksvr.dll",
    "estlib.dll",
    "estor.dll",
    "etel.dll",
    "etel3rdparty.dll",
    "etext.dll",
    "euser.dll",
    "exiflib.dll",
    "ezip.dll",
    "ezlib.dll",
    "f32agentui.dll",
    "favouritesengine.dll",
    "fbscli.dll",
    "featdiscovery.dll",
    "fepbase.dll",
    "field.dll",
    "flogger.dll",
    "fntstr.dll",
    "fontutils.dll",
    "form.dll",
    "ftpprot.dll",
    "ftpsess.dll",
    "gb2312_shared.dll",
    "gdi.dll",
    "gfp.dll",
    "gifscaler.dll",
    "grid.dll",
    "gsmu.dll",
    "hal.dll",
    "hash.dll",
    "hlplch.dll",
    "hlpmodel.dll",
    "http.dll",
    "hwrmlightclient.dll",
    "hwrmvibraclient.dll",
    "imageconversion.dll",
    "imagetransform.dll",
    "imcm.dll",
    "imps.dll",
    "imut.dll",
    "inetprotutil.dll",
    "insock.dll",
    "irc.dll",
    "irda.dll",
    "irobex.dll",
    "irs.dll",
    "irtranp.dll",
    "jisx0201.dll",
    "jisx0208.dll",
    "jpegyuvdecoder.dll",
    "lbs.dll",
    "libc.dll",
    "libcrypt.dll",
    "libcrypto.dll",
    "libdl.dll",
    "libgles_cm.dll",
    "libglib.dll",
    "libgmodule.dll",
    "libgobject.dll",
    "libgthread.dll",
    "liblogger.dll",
    "libm.dll",
    "libpthread.dll",
    "libssl.dll",
    "libz.dll",
    "linebreak.dll",
    "listenerdopplereffect.dll",
    "listenerlocationeffect.dll",
    "listenerorientationeffect.dll",
    "lmkcommonui.dll",
    "locationbase.dll",
    "logcli.dll",
    "logwrap.dll",
    "loudnesseffect.dll",
    "mediaclient.dll",
    "mediaclientaudio.dll",
    "mediaclientaudioinputstream.dll",
    "mediaclientaudiostream.dll",
    "mediaclientimage.dll",
    "mediaclientvideo.dll",
    "mgfetch.dll",
    "midiclient.dll",
    "mmfcontrollerframework.dll",
    "mmfstandardcustomcommands.dll",
    "mmscli.dll",
    "msgeditorutils.dll",
    "msgs.dll",
    "mtur.dll",
    "netmeta.dll",
    "nifman.dll",
    "npdlib.dll",
    "numberconversion.dll",
    "obexclientmtm.dll",
    "obexmtmutil.dll",
    "obexservermtm.dll",
    "ocrsrv.dll",
    "orientationbase.dll",
    "palette.dll",
    "pbkeng.dll",
    "pbkview.dll",
    "pdrprt.dll",
    "pdrstr.dll",
    "pkcs10.dll",
    "pkixcert.dll",
    "platformenv.dll",
    "platformver.dll",
    "pops.dll",
    "powermgrcli.dll",
    "prev.dll",
    "print.dll",
    "profileengine.dll",
    "ptiengine.dll",
    "pushmsgentry.dll",
    "random.dll",
    "redircli.dll",
    "remconclient.dll",
    "remconcoreapi.dll",
    "remconextapi1.dll",
    "remconinterfacebase.dll",
    "remcontypes.dll",
    "richbio.dll",
    "roomleveleffect.dll",
    "rtp.dll",
    "satinfo.dll",
    "scdv.dll",
    "schsvr.dll",
    "scppnwdl.dll",
    "sdpagent.dll",
    "sdpcodec.dll",
    "sdpdatabase.dll",
    "securesocket.dll",
    "sendas2.dll",
    "sendui.dll",
    "senservconn.dll",
    "senservdesc.dll",
    "senservmgr.dll",
    "senutils.dll",
    "senxml.dll",
    "servicehandler.dll",
    "sipclient.dll",
    "sipcodec.dll",
    "sipprofilecli.dll",
    "smcm.dll",
    "smss.dll",
    "smts.dll",
    "sourcedopplereffect.dll",
    "sourcelocationeffect.dll",
    "sourceorientationeffect.dll",
    "spdctrl.dll",
    "sqldb.dll",
    "ssl.dll",
    "stereowideningeffect.dll",
    "stereowideningutility.dll",
    "sysutil.dll",
    "tagma.dll",
    "telsess.dll",
    "timezonelocalization.dll",
    "tzclient.dll",
    "uiklaf.dll",
    "undo.dll",
    "vcal.dll",
    "vcard.dll",
    "versit.dll",
    "vibractrl.dll",
    "viewcli.dll",
    "wapmsgcli.dll",
    "wapp.dll",
    "wappushutils.dll",
    "watcher.dll",
    "wnode.dll",
    "worldclient.dll",
    "wpeng.dll",
    "ws32.dll",
    "wtlscert.dll",
    "wutil.dll",
    "x500.dll",
    "x509.dll",
    "xmlframework.dll",
];

/// Names with no import library in this SDK, asked anyway.
///
/// The SDK ships a `.dso` only for what it expects an application to link. A handset
/// carries more than that — and the gap is exactly where the interesting questions live:
/// `etelmm.dll` holds `RMobilePhone`, which is how anything learns the operator, the
/// signal or the IMEI, and there is no import library for it here at all.
///
/// A sweep generated purely from the SDK would therefore report on what Nokia expected us
/// to use rather than on what the phone has. These close that gap.
pub const EXTRA: &[&str] = &[
    // RMobilePhone lives here: operator, signal strength, IMEI. There is no etelmm.dso in
    // this SDK at all, so nothing could link it today even if the handset has it.
    "etelmm.dll",
    "etelpckt.dll",
    // The sensor framework's client. Also absent from the SDK's import libraries.
    "sensrvclient.dll",
    "sensrvutil.dll",
    "httpfilterauthentication.dll",
    "httpfiltercommon.dll",
    "locationmanager.dll",
];

/// Controls: names that must load on any Symbian handset.
///
/// Their job is to catch a broken instrument. If `euser.dll` comes back absent then the
/// query itself is failing rather than the device being bare — and without a control, a
/// sweep that reported *everything* missing would read as a devastating finding instead
/// of as a bug.
pub const CONTROLS: &[&str] = &["euser.dll", "avkon.dll", "efsrv.dll"];

/// Names worth calling out in the report even though they are in [`NAMES`], because a
/// decision is waiting on each one.
pub const NOTABLE: &[(&str, &str)] = &[
    ("libssl.dll", "TLS - the entire TLS story on this handset is whether this loads"),
    ("libcrypto.dll", "OpenSSL 0.9.8a: AES, RSA, bignum - but no SHA-256, which predates it"),
    ("libc.dll", "Open C: BSD sockets and stdio"),
    ("libz.dll", "inflate"),
    ("msgs.dll", "the Message Server - what an MTM would be built on"),
    ("mtur.dll", "the MTM registries"),
    ("etel.dll", "telephony"),
    ("etelmm.dll", "RMobilePhone: operator, signal, IMEI"),
    ("sensrvclient.dll", "the sensor framework"),
    ("btdevice.dll", "Bluetooth"),
    ("lbs.dll", "location"),
    ("centralrepository.dll", "central repository"),
    ("edbms.dll", "DBMS"),
    ("http.dll", "the platform's own HTTP stack"),
    ("securesocket.dll", "the platform's own TLS sockets"),
    ("sqldb.dll", "Symbian SQL"),
    ("random.dll", "CSystemRandom, a real CSPRNG"),
    ("fepbase.dll", "the FEP base, for the keyboard's Fn layer"),
];

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::BTreeSet;

    /// Regenerate with:
    ///
    /// ```sh
    /// ls sdk/epoc32/release/armv5/lib/*.dso | xargs -n1 basename | sed 's/\.dso$//' \
    ///   | grep -v '{' | tr 'A-Z' 'a-z' | sort -u
    /// ```
    #[test]
    fn the_list_is_deduplicated_and_sorted() {
        let mut seen = BTreeSet::new();
        for n in NAMES {
            assert!(seen.insert(*n), "duplicate {n}");
            assert!(n.ends_with(".dll"), "{n} is not a DLL name");
            assert_eq!(*n, n.to_lowercase(), "{n} is not lower-cased");
        }
        let sorted: alloc::vec::Vec<_> = seen.into_iter().collect();
        assert_eq!(sorted.as_slice(), NAMES);
    }

    /// A sweep with no control cannot tell "the handset is bare" from "the query is
    /// broken", and the first reads as a far more dramatic finding than it is.
    #[test]
    fn the_controls_are_in_the_sweep() {
        for c in CONTROLS {
            assert!(NAMES.contains(c), "control {c} is not in the sweep");
        }
    }

    /// A notable name that is not swept would print an annotation next to a question
    /// nobody asked.
    #[test]
    fn every_notable_name_is_actually_swept() {
        for (n, why) in NOTABLE {
            assert!(
                NAMES.contains(n) || EXTRA.contains(n),
                "{n} is annotated but never asked about"
            );
            assert!(!why.is_empty());
        }
    }

    /// A name in both lists would be asked about twice and reported twice, and the second
    /// answer would read as a second fact.
    #[test]
    fn extra_names_are_not_already_in_the_sweep() {
        for n in EXTRA {
            assert!(!NAMES.contains(n), "{n} is in both NAMES and EXTRA");
            assert!(n.ends_with(".dll"));
        }
    }

    /// The sweep is the master key for every later "can we link this?" decision, so it
    /// being accidentally truncated is worth failing over.
    #[test]
    fn the_sweep_is_the_whole_sdk() {
        assert!(NAMES.len() > 250, "only {} names - was the list truncated?", NAMES.len());
    }
}
