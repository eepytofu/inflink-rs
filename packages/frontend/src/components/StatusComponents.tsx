/**
 * @fileoverview
 * 一些辅助性的小组件
 */

import { Loader2 } from "lucide-react";
import { useT } from "../i18n";
import { Alert } from "./Alert";
import styles from "./StatusComponents.module.css";

export function LoadingIndicator() {
	const t = useT();
	return (
		<div className={styles.loadingContainer}>
			<Loader2 className={styles.spinner} size={24} />
			<span className={styles.loadingText}>{t.loading}</span>
		</div>
	);
}

export function InitializationErrorAlert({ error }: { error: Error | null }) {
	const t = useT();
	return (
		<Alert severity="error" title={t.initFailedTitle}>
			{t.initFailedBody}
			<br />
			{t.errorMessage}: {error?.message || t.unknownError}
		</Alert>
	);
}
