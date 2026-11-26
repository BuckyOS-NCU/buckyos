import { useEffect, useState } from 'react'
import { Alert, CircularProgress } from '@mui/material'
import { fetchUserDetail, type SchoolProfile } from '../../../../api/user_mgr'
import { useI18n } from '../../../../i18n/provider'
import { getBootIdentity, isSchoolAccount } from '../../../../sso/identity'
import { InfoFieldsSection } from './InfoFieldsSection'

type ProfileState =
  | { kind: 'loading' }
  | { kind: 'unavailable' }
  | { kind: 'empty' }
  | { kind: 'ready'; profile: SchoolProfile }

export function SchoolProfileSection({ standalone = false }: { standalone?: boolean }) {
  const identity = getBootIdentity()
  const { t } = useI18n()
  if (!isSchoolAccount(identity)) {
    return standalone ? <Alert severity="info">{t('school.profile.noAccount', 'This account has no school information.')}</Alert> : null
  }
  return <SchoolProfileReader key={identity.user_id} userId={identity.user_id} />
}

function SchoolProfileReader({ userId }: { userId: string }) {
  const { t } = useI18n()
  const [state, setState] = useState<ProfileState>({ kind: 'loading' })

  useEffect(() => {
    let active = true
    void fetchUserDetail().then(({ data }) => {
      if (!active) return
      if (!data || data.user_id !== userId) setState({ kind: 'unavailable' })
      else if (!data.school_profile) setState({ kind: 'empty' })
      else setState({ kind: 'ready', profile: data.school_profile })
    }).catch(() => {
      if (active) setState({ kind: 'unavailable' })
    })
    return () => { active = false }
  }, [userId])

  if (state.kind === 'loading') return <CircularProgress size={20} aria-label={t('school.profile.loading', 'Loading school information')} />
  if (state.kind === 'unavailable') return <Alert severity="error">{t('school.profile.unavailable', 'School information is unavailable. Try again later.')}</Alert>
  if (state.kind === 'empty') return <Alert severity="info">{t('school.profile.empty', 'After your next school sign-in, verified information will appear here when available.')}</Alert>

  const profile = state.profile
  const fields: Record<string, string> = {
    ...(profile.real_name && { ncuRealName: profile.real_name }),
    [profile.school_number_type === 'staff' ? 'ncuStaffNumber' : 'ncuStudentNumber']: profile.school_number,
    ncuRole: profile.user_type_label || (profile.role === 'teacher' ? t('school.profile.teacher', 'Teacher') : t('school.profile.student', 'Student')),
    ...(profile.college_name && { ncuCollege: profile.college_name }),
    ...(profile.major_name && { ncuMajor: profile.major_name }),
    ...(profile.class_name && { ncuClass: profile.class_name }),
    ...(profile.grade && { ncuGrade: profile.grade }),
    ...(profile.degree_level && { ncuDegreeLevel: profile.degree_level }),
    ...(profile.current_status && { ncuStatus: profile.current_status }),
    ...(profile.title && { ncuTitle: profile.title }),
  }

  return (
    <section aria-label={t('school.profile.section', 'Verified school information')} data-testid="school-profile">
      <InfoFieldsSection title={t('school.profile.section', 'Verified school information')} fields={fields} editable={false} />
    </section>
  )
}
