/* netsurfprobe — does the NetSurf MIT stack link and run on this handset?
 *
 * WHY IT LOOKS LIKE THIS
 *
 * F5 of docs/plan-browser.md asks one question — can libwapcaplet, libparserutils,
 * libhubbub, libdom and libcss be cross-compiled and linked for armv5 — and the whole
 * risk (R1) is the C runtime. So this binary carries the five archives, the Open C
 * `libc` import they need, and nothing else: three imports total.
 *
 * That is the "one risky import set per binary" rule from docs/device-notes.md applied
 * to a set nothing here has imported before. It is also why there is no shim:
 * shim_app.cpp would add fourteen imports whose only contribution to this experiment is
 * fourteen more ways for the loader to refuse the image — and a refused image on this
 * platform presents as the icon doing nothing, with no panic and no log.
 *
 * It is HEADLESS, and that is the httpprobe lesson rather than a preference: a GUI
 * application is one instance per UID3, so a run that dies leaving its window group
 * behind makes the *next* launch exit immediately with no report to say why — a failure
 * indistinguishable from a binary that does not load at all.
 *
 * THIS FILE INCLUDES NO NETSURF HEADER, AND CANNOT
 *
 * They are C99 and not C++-safe: libdom names a parameter `namespace`, libcss declares
 * `*restrict`. Both are hard errors under g++. src/netsurf_probe.c owns every call into
 * the libraries and inc/netsurf_probe.h is the boundary. See that header.
 *
 * WHAT IT PROVES AND WHAT IT DOES NOT
 *
 * On the host: that 444 translation units compile and the link closes — the F5 exit
 * criterion for this workstream. On the handset: whether Open C's malloc actually serves
 * an allocation-heavy C library from inside a Symbian EXE, and whether the parsers give
 * the right answers on ARM. Untested; no handset access in this workstream. This report
 * file is the artefact that will answer it.
 */

#include <e32base.h>
#include <f32file.h>

#include "netsurf_probe.h"

/* Sized from the checks netsurf_probe.c actually records (26 at the time of writing),
 * with room to add a few without touching this file. Overflow is silent by design on the
 * C side — it stops recording rather than writing past the array — so the count in the
 * END line is what reveals it. */
static const TInt KMaxChecks = 48;

class TReport
	{
public:
	TReport() : iOk(0), iFail(0) {}

	void OpenL(RFs& aFs);
	void Close();
	void Line(const TDesC8& aLine);
	void Check(const netsurf_check& aCheck);
	TInt Ok() const { return iOk; }
	TInt Fail() const { return iFail; }

private:
	RFile iFile;
	TInt iOk;
	TInt iFail;
	};

void TReport::OpenL(RFs& aFs)
	{
	aFs.MkDirAll(_L("C:\\Data\\"));
	User::LeaveIfError(iFile.Replace(aFs, _L("C:\\Data\\netsurfprobe.txt"),
			EFileWrite | EFileShareAny));
	}

void TReport::Close()
	{
	iFile.Close();
	}

void TReport::Line(const TDesC8& aLine)
	{
	iFile.Write(aLine);
	iFile.Write(_L8("\r\n"));
	/* Flushed per line. The interesting failure is a probe that dies halfway, and a
	 * buffered writer would lose exactly the line that says where it died. */
	iFile.Flush();
	}

void TReport::Check(const netsurf_check& aCheck)
	{
	TBuf8<200> line;
	TPtrC8 name((const TUint8*) aCheck.name, User::StringLength(
			(const TUint8*) aCheck.name));

	/* Three line shapes, matching the grammar crates/symbian-report defines so the
	 * launcher's reader and a human's eye see the same file: `  ok  `, `  FAIL` and
	 * `  .   ` for a measurement with no verdict. */
	if (aCheck.verdict < 0)
		{
		line.Format(_L8("  .    %S  %d"), &name, aCheck.detail);
		}
	else if (aCheck.verdict > 0)
		{
		iOk++;
		line.Format(_L8("  ok   %S  %d"), &name, aCheck.detail);
		}
	else
		{
		iFail++;
		line.Format(_L8("  FAIL %S  %d"), &name, aCheck.detail);
		}
	Line(line);
	}

static void RunL()
	{
	RFs fs;
	User::LeaveIfError(fs.Connect());
	CleanupClosePushL(fs);

	TReport report;
	report.OpenL(fs);

	report.Line(_L8("== BEGIN netsurf"));
	report.Line(_L8(""));

	/* On the stack, not the heap, and not file-scope. Not tidiness: elf2e32 sets
	 * KImageNoCallEntryPoint unconditionally, so no static constructor in anything this
	 * toolchain builds ever runs (see the header of apps/dlltest/src/dlltest.cpp), and
	 * a file-scope array here would also be writable static data in an image that has
	 * no reason to have any. */
	netsurf_check checks[KMaxChecks];
	TInt n = netsurf_probe_run(checks, KMaxChecks);

	if (n < 0)
		{
		report.Line(_L8("  FAIL netsurf_probe_run refused the buffer"));
		}
	else
		{
		for (TInt i = 0; i < n; i++)
			{
			if (checks[i].section != NULL)
				{
				TPtrC8 sec((const TUint8*) checks[i].section,
						User::StringLength(
							(const TUint8*) checks[i].section));
				TBuf8<80> head;
				head.Format(_L8("== %S"), &sec);
				report.Line(head);
				}
			report.Check(checks[i]);
			}
		}

	TBuf8<80> tail;
	tail.Format(_L8("== END netsurf ok=%d fail=%d"), report.Ok(), report.Fail());
	report.Line(tail);
	report.Close();

	CleanupStack::PopAndDestroy(&fs);
	}

GLDEF_C TInt E32Main()
	{
	__UHEAP_MARK;
	CTrapCleanup* cleanup = CTrapCleanup::New();
	if (cleanup == NULL)
		return KErrNoMemory;

	TRAPD(err, RunL());

	delete cleanup;
	/* Deliberate. The five libraries allocate through Open C's malloc, which on this
	 * platform is the process heap — so if any of them leaks, this panics and the
	 * missing END line in the report is the evidence. A leak check is worth having in
	 * the one binary whose entire subject is whether a foreign allocator behaves. */
	__UHEAP_MARKEND;
	return err;
	}
