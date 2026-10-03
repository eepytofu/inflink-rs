import { useAtom } from "jotai";
import { ExternalLink } from "lucide-react";
import { useEffect, useState } from "react";
import type { NcmVersionInfo } from "@/hooks";
import { useT } from "@/i18n";
import { ignoredVersionAtom } from "@/store";
import { Alert } from "../Alert";
import { AnimatedLink } from "../AnimatedLink";
import { Collapse } from "../Collapse";

interface VersionWarningAlertProps {
	version: NcmVersionInfo | null;
	show: boolean;
}

export function VersionWarningAlert({
	version,
	show,
}: VersionWarningAlertProps) {
	const t = useT();
	const [ignoredVersion, setIgnoredVersion] = useAtom(ignoredVersionAtom);
	const [open, setOpen] = useState(false);

	useEffect(() => {
		if (show && version && version.raw !== ignoredVersion) {
			setOpen(true);
		} else {
			setOpen(false);
		}
	}, [show, version, ignoredVersion]);

	if (!show || !version) return null;

	const handleClose = () => {
		setOpen(false);
		setTimeout(() => {
			setIgnoredVersion(version.raw);
		}, 300);
	};

	return (
		<Collapse in={open} style={{ marginBottom: open ? "1rem" : 0 }}>
			<Alert
				severity="warning"
				title={t.versionWarningTitle}
				onClose={handleClose}
			>
				{t.versionWarningBody(version.raw)}
				<br />
				<br />
				<AnimatedLink
					onClick={() => {
						betterncm.ncm.openUrl(
							"https://github.com/apoint123/inflink-rs?tab=readme-ov-file#%E5%B7%B2%E6%B5%8B%E8%AF%95%E7%9A%84%E7%BD%91%E6%98%93%E4%BA%91%E9%9F%B3%E4%B9%90%E7%89%88%E6%9C%AC",
						);
					}}
					icon={<ExternalLink size={14} strokeWidth={2.5} />}
				>
					{t.versionWarningLink}
				</AnimatedLink>
			</Alert>
		</Collapse>
	);
}
