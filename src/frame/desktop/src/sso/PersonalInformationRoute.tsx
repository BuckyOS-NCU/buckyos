import { Button } from '@mui/material'
import { SchoolProfileSection } from '../app/users-agents/components/sections/SchoolProfileSection'
import { AuthPageFrame } from '../auth/AuthPageFrame'
import { useI18n } from '../i18n/provider'
import { useDocumentTitle } from '../presentation/documentHead'

export default function PersonalInformationRoute() {
  const { t } = useI18n()
  const title = t('school.profile.title', 'Personal information')
  useDocumentTitle(title)
  return (
    <AuthPageFrame maxWidth="sm" title={title}>
      <div className="space-y-4">
        <SchoolProfileSection standalone />
        <Button href="/" variant="text">{t('school.profile.home', 'Back to home')}</Button>
      </div>
    </AuthPageFrame>
  )
}
